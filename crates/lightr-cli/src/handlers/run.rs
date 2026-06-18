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
//! NOTE: Engine runs (engine != native OR rootfs given) are NOT memoized.
//! Only the default path (native + no rootfs) uses run_memoized.

use std::io::Write;

use lightr_core::{validate_ref_name, ResourceLimits};
use lightr_engine::{engine_for, EngineKind, ExecSpec};
use lightr_index;
use lightr_run::{
    run_memoized_deep, run_memoized_with, spawn_detached, DeepMemoConfig, Mount, PortMap, RunSpec,
    StoreFile,
};
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

#[derive(Serialize)]
struct RunJson {
    key: String,
    hit: bool,
    exit_code: i32,
}

/// Parse a raw "ref:target" mount string into (ref_name, target).
/// Returns Err(exit_code) on validation failure (already printed to stderr).
fn parse_mount(raw: &str) -> Result<Mount, i32> {
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
fn parse_publish(raw: &str) -> Result<PortMap, i32> {
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
    // Healthcheck flags are parsed at the CLI surface (A0.5) but the probe is
    // wired by WP-A3; accepted here as an honest no-op so the surface is frozen.
    _health_cmd: Option<&str>,
    _health_interval: u64,
    _health_retries: u32,
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
        // 2. Publishing is wired only for the native detached path. vz/ns
        //    networking is Phase 2.
        if use_engine_path {
            eprintln!(
                "lightr: -p/--publish is wired for the native detached path; --engine vz/ns networking is Phase 2"
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

    if use_engine_path {
        // ── Engine path (non-memoized) ────────────────────────────────────────
        // Hydrate rootfs ref into a temp dir if provided
        let rootfs_tmp: Option<tempfile::TempDir>;
        let rootfs_path: Option<std::path::PathBuf>;

        if let Some(ref_name) = rootfs_ref {
            let tmp = match tempfile::TempDir::new() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("lightr: run: cannot create temp dir for rootfs: {e}");
                    return 1;
                }
            };
            if let Err(e) = lightr_index::hydrate(tmp.path(), &store, ref_name) {
                return die_lightr(&e);
            }
            rootfs_path = Some(tmp.path().to_path_buf());
            rootfs_tmp = Some(tmp);
        } else {
            rootfs_tmp = None;
            rootfs_path = None;
        }

        let engine = match engine_for(engine_kind) {
            Ok(e) => e,
            Err(e) => return die_lightr(&e),
        };

        let spec = ExecSpec {
            cwd: &cwd,
            command,
            rootfs: rootfs_path.as_deref(),
            limits,
        };

        let code = match engine.run(&spec) {
            Ok(c) => c,
            Err(e) => return die_lightr(&e),
        };

        // Keep temp dir alive until after engine.run completes
        drop(rootfs_tmp);

        return code;
    }

    // ── Memoized path (native + no rootfs — unchanged R0/R1 behaviour) ────────

    let input_paths: Vec<std::path::PathBuf> = if inputs.is_empty() {
        vec![cwd.clone()]
    } else {
        inputs.iter().map(std::path::PathBuf::from).collect()
    };

    // Parse published ports (Phase 1). Policy above already guaranteed this is
    // the native detached path when `publish_raw` is non-empty. Empty ⇒ no-op,
    // so the non-published path is byte-identical to before.
    let mut ports: Vec<PortMap> = Vec::new();
    for raw in publish_raw {
        match parse_publish(raw) {
            Ok(p) => ports.push(p),
            Err(code) => return code,
        }
    }

    let spec = RunSpec {
        cwd,
        inputs: input_paths,
        command: command.to_vec(),
        env_keys: env_keys.to_vec(),
        mounts,
        secrets,
        configs,
        ports,
    };

    // Detach path: spawn detached and print the run id
    if detach {
        match spawn_detached(&spec, &store) {
            Ok(handle) => {
                println!("id={}", handle.id);
                return 0;
            }
            Err(e) => return die_lightr(&e),
        }
    }

    if explain {
        let os_arch = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        eprintln!(
            "lightr: explain run: inputs={} argv={} env={} os-arch={}",
            spec.inputs.len(),
            spec.command.len(),
            spec.env_keys.len(),
            os_arch
        );
    }

    // Deep-memo (opt-in): surface the honest capability note, then run.
    // The fn falls back to whole-run memo when the shim can't attach.
    let outcome = if deep_memo {
        let (avail, reason) = lightr_run::deep_memo_available();
        if !avail {
            eprintln!("lightr: deep-memo unavailable ({reason}) — falling back to whole-run memo");
        }
        match run_memoized_deep(&spec, &store, &DeepMemoConfig { enabled: true }) {
            Ok(o) => o,
            Err(e) => return die_lightr(&e),
        }
    } else {
        match run_memoized_with(&spec, &store, &limits) {
            Ok(o) => o,
            Err(e) => return die_lightr(&e),
        }
    };

    let hex = outcome.key.to_hex();
    let short = &hex[..16];
    let hit_str = if outcome.hit { "HIT" } else { "MISS" };
    eprintln!("lightr: memo {hit_str} key={short}");

    // Stream stdout then stderr raw (lossless).
    {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(&outcome.stdout).ok();
    }
    {
        let stderr = std::io::stderr();
        let mut err = stderr.lock();
        err.write_all(&outcome.stderr).ok();
    }

    if json {
        let obj = RunJson {
            key: hex.clone(),
            hit: outcome.hit,
            exit_code: outcome.exit_code,
        };
        eprintln!(
            "lightr-json: {}",
            serde_json::to_string(&obj).expect("serialize run")
        );
    }

    outcome.exit_code
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{parse_mount, parse_publish, run};

    // ── parse_publish ───────────────────────────────────────────────────────

    #[test]
    fn publish_parses_host_container() {
        let p = parse_publish("8080:80").expect("should parse");
        assert_eq!(p.host, 8080);
        assert_eq!(p.container, 80);
    }

    #[test]
    fn publish_accepts_explicit_tcp() {
        let p = parse_publish("39000:39001/tcp").expect("should parse");
        assert_eq!(p.host, 39000);
        assert_eq!(p.container, 39001);
    }

    #[test]
    fn publish_rejects_udp_as_phase2() {
        let r = parse_publish("8080:80/udp");
        assert!(r.is_err());
        assert_eq!(r.err().unwrap(), 2);
    }

    #[test]
    fn publish_rejects_missing_colon() {
        assert_eq!(parse_publish("8080").err().unwrap(), 2);
    }

    #[test]
    fn publish_rejects_zero_port() {
        assert_eq!(parse_publish("0:80").err().unwrap(), 2);
        assert_eq!(parse_publish("80:0").err().unwrap(), 2);
    }

    #[test]
    fn publish_rejects_out_of_range_and_nonnumeric() {
        // 70000 > u16::MAX ⇒ parse fails ⇒ Err(2).
        assert_eq!(parse_publish("70000:80").err().unwrap(), 2);
        assert_eq!(parse_publish("8080:abc").err().unwrap(), 2);
    }

    // ── policy guards (return 2 BEFORE any store/engine work) ─────────────────

    #[test]
    fn publish_without_detach_exits_2() {
        // -p given, detach=false ⇒ exit 2 (guard 1), before Store::open.
        let code = run(
            ".",
            &[],
            &[],
            &["true".to_string()],
            false, // json
            false, // explain
            false, // detach  ← NOT detached
            &["39000:39001".to_string()],
            &[],
            "native",
            None,
            false,
            None,
            None,
            &[],
            &[],
            None,
            30,
            3,
        );
        assert_eq!(code, 2, "-p without -d must exit 2");
    }

    #[test]
    fn publish_on_engine_path_exits_2() {
        // -p + -d but engine=vz ⇒ exit 2 (guard 2), before the engine early
        // return / any store work.
        let code = run(
            ".",
            &[],
            &[],
            &["true".to_string()],
            false,
            false,
            true, // detach
            &["39000:39001".to_string()],
            &[],
            "vz", // engine path ⇒ Phase 2
            None,
            false,
            None,
            None,
            &[],
            &[],
            None,
            30,
            3,
        );
        assert_eq!(code, 2, "-p on the engine path must exit 2 (Phase 2)");
    }

    // ── parse_mount (existing) ────────────────────────────────────────────────

    #[test]
    fn mount_parse_splits_on_first_colon() {
        let m = parse_mount("myref:some/target").expect("should parse");
        assert_eq!(m.ref_name, "myref");
        assert_eq!(m.target, "some/target");
    }

    #[test]
    fn mount_parse_splits_on_first_colon_extra_colons() {
        // "ref:sub:extra" → ref_name="ref", target="sub:extra" (split on FIRST colon)
        let m = parse_mount("ref:sub:extra").expect("should parse");
        assert_eq!(m.ref_name, "ref");
        assert_eq!(m.target, "sub:extra");
    }

    #[test]
    fn mount_rejects_absolute_target() {
        let result = parse_mount("ref:/abs/path");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), 2);
    }

    #[test]
    fn mount_rejects_invalid_ref_name() {
        // Uppercase ref name is invalid
        let result = parse_mount("INVALID:target");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), 2);
    }

    #[test]
    fn mount_rejects_missing_colon() {
        let result = parse_mount("nocoton");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), 2);
    }

    #[test]
    fn mount_accepts_relative_target() {
        let m = parse_mount("valid-ref:sub/dir").expect("should parse");
        assert_eq!(m.ref_name, "valid-ref");
        assert_eq!(m.target, "sub/dir");
    }
}
