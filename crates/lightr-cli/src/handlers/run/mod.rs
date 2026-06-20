//! `lightr run` handler — build-spec v2 §7 + build-spec-r1 §4 + build-spec-r2 §4.
//!
//! Exit = child's exit code.
//!
//! Stderr memo marker BEFORE streaming outputs:
//!   `lightr: memo HIT key=<hex16>` or `lightr: memo MISS key=<hex16>`
//!
//! Streaming: write stdout bytes to stdout, stderr bytes to stderr (raw, lossless).
//!
//! --json: raw child streams still flow; a JSON object `{"key","hit","exit_code"}`
//!         goes to a final line on STDERR prefixed `lightr-json: ` (machine readable
//!         without corrupting child stdout). exit = outcome.exit_code.
//!
//! --explain: extra stderr lines prefixed `lightr: explain `
//!   for run: the key composition counts (inputs n, argv n, env n, os-arch).
//!
//! --detach: spawn a detached run; print id=<handle.id>; exit 0.
//! --mount REF:TARGET: mount a ref into the run's cwd at TARGET (relative).
//!
//! --engine native|ns|vz (default native): pick the execution engine.
//! --rootfs <ref>: hydrate the named ref CoW into a temp dir and hand it
//!   to the engine as the rootfs. Incompatible with native (exit 2).
//!
//! Memoized paths: (a) native + no rootfs → run_memoized (R0/R1); (b) vz +
//! rootfs + NOT detached → run_vz_memoized (the vz-memo moat) — the 1st run
//! boots the VM + captures {exit, stdout, stderr}, an identical 2nd run is an
//! Action-Cache HIT replayed with NO VM boot. All other engine runs (ns/wsl, vz
//! detached, vz without rootfs) are NOT memoized and take the plain engine path.

use lightr_core::{validate_ref_name, ResourceLimits};
use lightr_engine::EngineKind;
use lightr_run::healthcheck::Healthcheck;
use lightr_run::{spawn_detached_engine, Mount, PortMap, RunSpec, StoreFile};
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

mod paths;

#[cfg(test)]
mod tests;

#[derive(Serialize)]
pub(super) struct RunJson {
    pub(super) key: String,
    pub(super) hit: bool,
    pub(super) exit_code: i32,
}

/// Parse a raw "ref:target" mount string into (ref_name, target).
/// Returns Err(exit_code) on validation failure (already printed to stderr).
pub(super) fn parse_mount(raw: &str) -> Result<Mount, i32> {
    // Split on FIRST ':' only
    let colon = raw.find(':').ok_or_else(|| {
        eprintln!("lightr: invalid --mount value (missing ':'): {raw}");
        2i32
    })?;
    let ref_name = &raw[..colon];
    let target = &raw[colon + 1..];

    // Validate ref name
    if let Err(e) = validate_ref_name(ref_name) {
        eprintln!("lightr: invalid mount ref name: {e}");
        return Err(2);
    }

    // Validate target is relative (not absolute)
    if target.starts_with('/') {
        eprintln!("lightr: mount target must be relative, got: {target}");
        return Err(2);
    }

    Ok(Mount {
        ref_name: ref_name.to_string(),
        target: target.to_string(),
    })
}

/// Parse a raw "NAME=REF" secret/config string into a `StoreFile`.
/// Returns Err(exit_code) on a missing '=' (already printed to stderr).
fn parse_store_file(raw: &str, kind: &str) -> Result<StoreFile, i32> {
    let eq = raw.find('=').ok_or_else(|| {
        eprintln!("lightr: invalid --{kind} value (missing '='): {raw}");
        2i32
    })?;
    let name = &raw[..eq];
    let ref_name = &raw[eq + 1..];
    if name.is_empty() || ref_name.is_empty() {
        eprintln!("lightr: invalid --{kind} value (expected NAME=REF): {raw}");
        return Err(2);
    }
    Ok(StoreFile {
        name: name.to_string(),
        ref_name: ref_name.to_string(),
    })
}

/// Parse a raw `-p/--publish` value into a `PortMap` (Networking Phase 1).
///
/// Accepts `HOST:CONTAINER` or `HOST:CONTAINER/tcp`. Both ports must parse as
/// u16 in `1..=65535`. `…/udp` is rejected (UDP publish is Phase 2). On any bad
/// input prints to stderr and returns `Err(2)` (mirrors `parse_mount`).
pub(super) fn parse_publish(raw: &str) -> Result<PortMap, i32> {
    // Strip an optional `/proto` suffix. Only tcp is supported in v1.
    let (body, proto) = match raw.rsplit_once('/') {
        Some((b, p)) => (b, Some(p)),
        None => (raw, None),
    };
    match proto {
        None | Some("tcp") => {}
        Some("udp") => {
            eprintln!("lightr: invalid -p/--publish value ({raw}): udp publish is Phase 2");
            return Err(2);
        }
        Some(other) => {
            eprintln!("lightr: invalid -p/--publish protocol '{other}' in {raw} (expected tcp)");
            return Err(2);
        }
    }

    let colon = body.find(':').ok_or_else(|| {
        eprintln!("lightr: invalid -p/--publish value (expected HOST:CONTAINER): {raw}");
        2i32
    })?;
    let host_str = &body[..colon];
    let container_str = &body[colon + 1..];

    let parse_port = |s: &str, which: &str| -> Result<u16, i32> {
        match s.parse::<u16>() {
            Ok(p) if (1..=65535).contains(&p) => Ok(p),
            _ => {
                eprintln!("lightr: invalid {which} port '{s}' in {raw} (expected 1..=65535)");
                Err(2)
            }
        }
    };

    let host = parse_port(host_str, "host")?;
    let container = parse_port(container_str, "container")?;
    Ok(PortMap { host, container })
}

/// The `--health-*` CLI flags, bundled (WP-RC-4). Built from the parsed `Cmd`
/// in dispatch and lowered to a [`Healthcheck`] by [`HealthFlags::build`].
///
/// `cmd == None` (no `--health-cmd`) OR `no_healthcheck == true` ⇒ no
/// healthcheck (the latter is Docker's `--no-healthcheck`, which wins over any
/// other `--health-*` flag). Otherwise the flags lower 1:1 to a [`Healthcheck`].
#[derive(Clone, Debug, Default)]
pub struct HealthFlags {
    pub cmd: Option<String>,
    pub interval: u64,
    pub timeout: u64,
    pub start_period: u64,
    pub retries: u32,
    pub no_healthcheck: bool,
}

impl HealthFlags {
    /// Lower the flags to a [`Healthcheck`], or `None` when no healthcheck is
    /// configured. `--no-healthcheck` disables unconditionally (Docker
    /// semantics); a missing `--health-cmd` is also "no healthcheck".
    pub fn build(&self) -> Option<Healthcheck> {
        if self.no_healthcheck {
            return None;
        }
        let cmd = self.cmd.clone()?;
        Some(Healthcheck {
            cmd,
            interval_s: self.interval,
            timeout_s: self.timeout,
            start_period_s: self.start_period,
            retries: self.retries,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    dir: &str,
    inputs: &[String],
    env_keys: &[String],
    command: &[String],
    json: bool,
    explain: bool,
    detach: bool,
    publish_raw: &[String],
    mounts_raw: &[String],
    engine_str: &str,
    rootfs_ref: Option<&str>,
    deep_memo: bool,
    memory: Option<&str>,
    cpus: Option<&str>,
    secrets_raw: &[String],
    configs_raw: &[String],
    // WP-RC-4: healthcheck flags, now WIRED (was parsed & discarded). Lowered to
    // a Healthcheck and run by the detached supervisor's watchdog. Never a
    // memo-key input (runtime probe, §0).
    health: &HealthFlags,
) -> i32 {
    // Parse engine kind — bad value ⇒ exit 2
    let engine_kind = match engine_str.parse::<EngineKind>() {
        Ok(k) => k,
        Err(e) => return die_lightr(&e),
    };

    // Parse resource caps (F-203). Malformed ⇒ exit 2 (fail closed).
    let limits = match ResourceLimits::parse(memory, cpus) {
        Ok(l) => l,
        Err(e) => return die_lightr(&e),
    };

    // Parse secrets/configs (F-309) — split NAME=REF.
    let mut secrets: Vec<StoreFile> = Vec::new();
    for raw in secrets_raw {
        match parse_store_file(raw, "secret") {
            Ok(sf) => secrets.push(sf),
            Err(code) => return code,
        }
    }
    let mut configs: Vec<StoreFile> = Vec::new();
    for raw in configs_raw {
        match parse_store_file(raw, "config") {
            Ok(sf) => configs.push(sf),
            Err(code) => return code,
        }
    }

    // Decide path: native + no rootfs ⇒ memoized path (unchanged R0/R1 behaviour).
    // Any other combination ⇒ engine path (NOT memoized, per §4).
    let use_engine_path = engine_kind != EngineKind::Native || rootfs_ref.is_some();

    // ── WP-RC-4: lower the --health-* flags to a Healthcheck ───────────────────
    // The healthcheck is a SUPERVISOR-owned watchdog: it only runs for detached
    // (`-d`) runs (the supervisor probes on the interval and writes the verdict
    // to <run_dir>/health for `ps`). For a non-detached run we have no
    // supervisor, so a configured `--health-cmd` is honestly reported as
    // supervisor-only rather than silently dropped — fail-open on the run itself
    // (the command still runs), fail-loud on the unmet expectation.
    let healthcheck = health.build();
    if healthcheck.is_some() && !detach {
        eprintln!(
            "lightr: --health-cmd is wired for detached runs only (-d); the \
             healthcheck watchdog is owned by the supervisor — running without it"
        );
    }

    // ── Networking Phase 1 policy (frozen, honest — enforce in this order) ────
    // These guards run BEFORE the engine-path early return below, so an
    // `--engine vz/ns -p ...` invocation hits the honest Phase-2 error rather
    // than silently dropping the published port.
    if !publish_raw.is_empty() {
        // 1. A published service is a long-running server ⇒ it must be detached.
        if !detach {
            eprintln!("lightr: -p/--publish requires -d (a published service runs detached)");
            return 2;
        }
        // 2. Publishing is wired for (a) the native detached path and (b) the vz
        //    detached container path (WP-NET2: `--engine vz --rootfs <img>`, host→
        //    guest forward). Every other engine (ns/wsl) + vz-without-rootfs is
        //    still Phase 2 — an honest error, never a silently dropped port.
        let native = engine_kind == EngineKind::Native && rootfs_ref.is_none();
        let vz_container = engine_kind == EngineKind::Vz && rootfs_ref.is_some();
        if !native && !vz_container {
            eprintln!(
                "lightr: -p/--publish is wired for the native and `--engine vz --rootfs` \
                 detached paths; other engines are Phase 2"
            );
            return 2;
        }
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    // Parse mounts
    let mut mounts: Vec<Mount> = Vec::new();
    for raw in mounts_raw {
        match parse_mount(raw) {
            Ok(m) => mounts.push(m),
            Err(code) => return code,
        }
    }

    let cwd = std::path::PathBuf::from(dir);

    // ── DISPATCH ──────────────────────────────────────────────────────────────

    // ── vz-memo path (the product's core moat) ────────────────────────────────
    // A `vz` container job with a rootfs that is NOT detached is MEMOIZABLE
    // exactly like the native path: the 1st run boots the VM + captures
    // {exit, stdout, stderr}; an identical 2nd run is a HIT that replays them
    // from the Action Cache with NO VM boot. `-d`, non-rootfs, and non-vz cases
    // fall through to the existing (non-memoized) engine path unchanged.
    if let (EngineKind::Vz, Some(ref_name), false) = (engine_kind, rootfs_ref, detach) {
        return paths::run_vz_memo(engine_kind, ref_name, command, &store, &cwd, limits, json);
    }

    // ── vz detached container path (WP-NET2) ──────────────────────────────────
    // A `vz` run WITH a rootfs that IS detached boots a Linux container in a
    // microVM under the supervisor, which forwards each published port to the
    // guest's DHCP IP (`-p` for a Linux image — the flagship Docker-parity case).
    // The non-detached vz+rootfs case is the memo path above; ns/native fall
    // through. This runs the VM detached (the old engine path ignored `-d` and
    // blocked synchronously) — `spawn_detached_engine` returns immediately.
    if let (EngineKind::Vz, Some(ref_name), true) = (engine_kind, rootfs_ref, detach) {
        let mut ports: Vec<PortMap> = Vec::new();
        for raw in publish_raw {
            match parse_publish(raw) {
                Ok(p) => ports.push(p),
                Err(code) => return code,
            }
        }
        let spec = RunSpec {
            cwd,
            inputs: Vec::new(),
            command: command.to_vec(),
            env_keys: env_keys.to_vec(),
            mounts,
            secrets,
            configs,
            ports,
        };
        return match spawn_detached_engine(
            &spec,
            &store,
            healthcheck.as_ref(),
            EngineKind::Vz,
            Some(ref_name),
            &[],
        ) {
            Ok(handle) => {
                println!("id={}", handle.id);
                0
            }
            Err(e) => die_lightr(&e),
        };
    }

    if use_engine_path {
        return paths::run_engine(engine_kind, rootfs_ref, &store, &cwd, command, limits);
    }

    // ── Memoized path (native + no rootfs — unchanged R0/R1 behaviour) ────────
    paths::run_native_memo(paths::NativeRun {
        inputs,
        publish_raw,
        command,
        env_keys,
        mounts,
        secrets,
        configs,
        cwd,
        detach,
        store: &store,
        explain,
        json,
        deep_memo,
        limits,
        healthcheck,
    })
}
