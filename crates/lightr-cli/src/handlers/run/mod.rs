//! `lightr run` handler — build-spec v2 §7 + build-spec-r1 §4 + build-spec-r2 §4.
//!
//! Exit = child's exit code. Stderr memo marker BEFORE streaming outputs:
//!   `lightr: memo HIT key=<hex16>` / `MISS`. Streaming: raw stdout/stderr bytes,
//!   lossless. `--json`: streams flow + a final STDERR `lightr-json: {…}` line
//!   (`key`/`hit`/`exit_code`). `--explain`: extra `lightr: explain ` key-counts.
//!
//! --detach: spawn a detached run; print id=<handle.id>; exit 0.
//! --mount REF:TARGET: mount a ref into the run's cwd at TARGET (relative).
//! WP-B2: `-p` ranges/host-ip + `-P/--publish-all` are wired end-to-end here.
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

use lightr_core::ResourceLimits;
use lightr_engine::EngineKind;
use lightr_run::{spawn_detached_engine, Mount, PortMap, RunSpec, StoreFile};
use lightr_store::Store;

use crate::exit::die_lightr;

mod env;
mod flags;
mod helpers;
mod paths;
mod runflags;

// Handler helpers split to `helpers.rs` (godfile cap). `claim_name_and_print` is
// also used by `paths.rs` via `super::`; `expose_port_maps` feeds the `-P` branch.
pub(super) use helpers::claim_name_and_print;
use helpers::expose_port_maps;

// Flag parsing + value types live in `flags.rs` (skeleton-split for headroom).
// Re-exported at the `run` module root so sibling files + tests reach them via
// `super::Item` / `super::super::Item` exactly as before (zero-diff siblings).
pub(super) use flags::{parse_mount, parse_store_file, resolve_net_isolate, RcConfig, RunJson};
// WP-B2: `parse_publish` (single-port wrapper) now has TEST-only callers — the
// run path consumes the range-aware `parse_publish_spec` via `flags::publish::`.
#[cfg(test)]
pub(super) use flags::publish::parse_publish;
pub use flags::{HealthFlags, RawRcFlags};
// WP-RUNFLAGS: `-v/--volume`, `--tmpfs`, `--name`, `--rm`, `--entrypoint` (+
// honest Phase-2 networking flags) bundle, resolved into RunSpec carry-fields.
pub use runflags::RawRunFlags;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_health;
#[cfg(test)]
mod tests_runflags;

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
    // WP-B2: `-P/--publish-all` — auto-publish the rootfs image's EXPOSE list
    // (TCP) alongside any `-p`. `false` ⇒ no auto-publish (byte-identical).
    publish_all: bool,
    mounts_raw: &[String],
    engine_str: &str,
    rootfs_ref: Option<&str>,
    // WP-NET-ISO: `--net host|none` (host=default, share host network).
    net_str: &str,
    deep_memo: bool,
    memory: Option<&str>,
    cpus: Option<&str>,
    secrets_raw: &[String],
    configs_raw: &[String],
    // WP-RC-1: `-e`/`--env-file` → KEYED `env_explicit` (R-KEY); long `--env` = `env_keys` discovery.
    env_set: &[String],
    env_file: Option<&str>,
    // RUNTIME-ONLY docker-parity flags (never keyed; `None` ⇒ today's behaviour).
    // WP-RC-WORKDIR `-w` (Docker WORKDIR; `None` ⇒ `dir`; CLI > image WORKDIR — WP-DF-IMGCFG).
    workdir: Option<&str>,
    // WP-RC-USER `-u` (`None` ⇒ current user): native child uid/gid (cfg(unix)).
    user: Option<&str>,
    // WP-RC-RESTART `--restart` (`None` ⇒ `no`): supervisor re-spawn loop; pre-validated.
    restart: Option<&str>,
    // WP-RC-STOPSIGNAL `--stop-signal` (`None` ⇒ SIGTERM): `lightr stop`; pre-validated.
    stop_signal: Option<&str>,
    // WP-RC-4: healthcheck flags, WIRED — lowered to a Healthcheck run by the
    // supervisor's watchdog. Never a memo-key input (runtime probe, §0).
    health: &HealthFlags,
    // WP-RC-FLAGS: the 11 run-config flags (raw clap values). Resolved (labels
    // KEY=VAL parsed, shm-size parsed) then lowered into RunSpec carry-fields.
    // RUNTIME-ONLY — none enters the memo key. All-default ⇒ no-op carry.
    rc: RawRcFlags,
    // WP-RUNFLAGS: `-v/--volume`, `--tmpfs`, `--name`, `--rm`, `--entrypoint` (+
    // honest Phase-2 networking flags). Resolved (binds parsed, entrypoint split,
    // network flags honest-errored) then lowered into RunSpec carry-fields.
    // RUNTIME-ONLY — none enters the memo key. All-default ⇒ no-op carry.
    runflags: RawRunFlags,
) -> i32 {
    // WP-RC-FLAGS: parse `--label`/`--shm-size` (fail-closed: bad value ⇒ exit 2).
    let rc = match rc.resolve() {
        Ok(c) => c,
        Err(code) => return code,
    };

    // WP-#92: SECURITY flags the rootless `ns` engine cannot honestly enforce are
    // HONEST-ERRORED here (exit 2) BEFORE any provisioning — never silently
    // recorded. A silent no-op on a security flag is worse than an error: the user
    // believes they are sandboxed and isn't. This fires for EVERY engine (native =
    // no sandbox by design; the ns userns already BOUNDS the capability set but
    // full capset management is a separate tracked WP; vz is tracked). Mirrors the
    // lead's vz+pids honest-error guard below.
    if rc.privileged {
        eprintln!(
            "lightr: --privileged is not supported on the rootless ns engine (no real \
             privilege in an unprivileged user namespace); tracked for --engine vz"
        );
        return 2;
    }
    // WP-#94: `--cap-add`/`--cap-drop` are REAL on the `ns` engine (it drops the
    // bounding set + capsets the desired set as the last step before exec). The
    // engine-aware guard fires AFTER `engine_kind` is parsed below; native/vz keep
    // the honest exit-2 (native = no sandbox; vz caps live inside the guest, not
    // managed by the shim).
    // WP-#95: `--init` is now ENFORCED on the `ns` engine (a real PID-1 reaper inside
    // the new pid namespace — see ExecSpec.init). The engine-aware honest note for
    // OTHER engines (native/vz, where it stays a recorded-only carry-slot) fires
    // AFTER `engine_kind` is parsed below.

    // WP-RUNFLAGS: parse `-v`/`--entrypoint` + honest-error the networking flags
    // (fail-closed: bad value / Phase-2 flag ⇒ exit 2).
    let runflags = match runflags.resolve() {
        Ok(f) => f,
        Err(code) => return code,
    };
    // WP-RUNFLAGS: `--name` + `--rm` need a run dir/id, which only the DETACHED
    // path creates (a foreground run is stateless — just the Action Cache). So
    // they are detached-only; using them without `-d` is an honest exit 2, never
    // a silent no-op.
    if runflags.name.is_some() && !detach {
        eprintln!("lightr: --name requires -d (a named run is a detached container)");
        return 2;
    }
    if runflags.rm && !detach {
        eprintln!("lightr: --rm requires -d (a foreground run leaves no run dir to remove)");
        return 2;
    }
    // Parse engine kind — bad value ⇒ exit 2
    let engine_kind = match engine_str.parse::<EngineKind>() {
        Ok(k) => k,
        Err(e) => return die_lightr(&e),
    };

    // WP-#94: capability enforcement is REAL only on the `ns` engine. For any
    // OTHER engine, `--cap-add`/`--cap-drop` are HONEST-ERRORED (exit 2) BEFORE
    // provisioning rather than silently recorded — native is no sandbox by design,
    // and vz capabilities live inside the guest (not managed by this shim). The ns
    // engine does NOT error here: it enforces the requested set in `run_engine`
    // (the desired set = full userns set − cap_drop + cap_add, applied as the last
    // step before exec). A silent no-op on a security flag would give false
    // security — the exact failure WP-#92 refused.
    if engine_kind != EngineKind::Ns && (!rc.cap_add.is_empty() || !rc.cap_drop.is_empty()) {
        eprintln!(
            "lightr: --cap-add/--cap-drop capability enforcement is implemented only on \
             the rootless ns engine (--engine ns); native is no sandbox and vz caps live \
             inside the guest — refusing to run rather than give false security"
        );
        return 2;
    }

    // WP-#106: `--apparmor` is REAL only on the `ns` engine (it applies the profile
    // via aa_change_onexec right before exec). For any OTHER engine it is
    // HONEST-ERRORED (exit 2) BEFORE provisioning — native is no sandbox by design,
    // and the vz LSM lives inside the guest (not managed by this shim). A silent
    // no-op on a security flag would give false security (the failure WP-#92 refused).
    if engine_kind != EngineKind::Ns && rc.apparmor.is_some() {
        eprintln!(
            "lightr: --apparmor (AppArmor LSM enforcement) is implemented only on the \
             rootless ns engine (--engine ns); native is no sandbox and the vz LSM lives \
             inside the guest — refusing to run rather than give false security"
        );
        return 2;
    }

    // WP-#95: `--init` is ENFORCED on the `ns` engine (real PID-1 reaper). On any
    // OTHER engine it is a recorded-only carry-slot (native is a host process with no
    // pid namespace; vz reaps via its own guest PID 1) — say so honestly rather than
    // imply a reaper that won't run.
    if rc.init && engine_kind != EngineKind::Ns {
        eprintln!(
            "lightr: note: --init runs a real PID-1 reaper only on the rootless ns \
             engine (--engine ns); here it is recorded only (no pid namespace to reap in)"
        );
    }

    // WP-NET-ISO: parse `--net` + enforce `none` has a netns (fail-closed, exit 2).
    let is_pure_native = engine_kind == EngineKind::Native && rootfs_ref.is_none();
    let net_isolate = match resolve_net_isolate(net_str, is_pure_native) {
        Ok(v) => v,
        Err(code) => return code,
    };

    // WP-NET3: vz container-networking flags require `--engine vz --rootfs <img>`
    // (helper owns the doctrine + honest exit 2; fail-closed BEFORE provisioning).
    if let Some(code) = helpers::network_flags_policy_error(&runflags, engine_kind, rootfs_ref) {
        return code;
    }

    // Parse resource caps (F-203). Malformed ⇒ exit 2 (fail closed). WP-#90:
    // fold in `--pids-limit` (cgroup v2 `pids.max`) — enforced on the `ns` engine.
    let limits = match ResourceLimits::parse(memory, cpus) {
        Ok(l) => l.with_pids(rc.pids_limit),
        Err(e) => return die_lightr(&e),
    };

    // WP-#90: a pids cap needs cgroup v2 `pids.max`, which only the `ns` engine
    // owns. vz is a microVM (no delegated per-container cgroup via the shim) — so a
    // `--pids-limit --engine vz` request is honest-errored HERE, before the VM
    // boots, rather than silently dropped. native is honest-errored at the engine
    // boundary (`check_native_support`); the carry-field stays recorded-only.
    if engine_kind == EngineKind::Vz && limits.pids_max.is_some() {
        eprintln!(
            "lightr: vz engine cannot enforce a pids limit (no per-container cgroup \
             in the microVM); use --engine ns"
        );
        return 2;
    }

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

    // WP-RC-1: `-e`/`--env-file` → KEYED env_explicit (R-KEY); file then `-e` overrides; `KEY`-only inherits process env; empty ⇒ key byte-identical.
    let env_explicit = match env::resolve_env_explicit_from_process(env_set, env_file) {
        Ok(pairs) => pairs,
        Err(code) => return code,
    };

    // Decide path: native + no rootfs ⇒ memoized path (unchanged R0/R1 behaviour).
    // Any other combination ⇒ engine path (NOT memoized, per §4).
    let use_engine_path = engine_kind != EngineKind::Native || rootfs_ref.is_some();

    // ── WP-RC-4: lower the --health-* flags to a Healthcheck ───────────────────
    // Healthcheck = supervisor watchdog (detached `-d` only); non-detached
    // `--health-cmd` is fail-loud supervisor-only, never silently dropped.
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
        // 2. Publishing is wired for the native detached path + the vz detached
        //    container path (WP-NET2: `--engine vz --rootfs <img>`); other engines
        //    + vz-without-rootfs are Phase 2 — an honest error, never a dropped port.
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
    // WP-B2: `-P/--publish-all` shape guard (detached vz container only; fail-closed).
    if publish_all {
        if let Some(c) = helpers::publish_all_policy_error(detach, engine_kind, rootfs_ref) {
            return c;
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
        // WP-B2: consume the range-aware, host-ip-carrying parser so
        // `-p 8000-8002:8000-8002` yields 3 PortMaps and `-p 127.0.0.1:H:C` binds
        // loopback. Then (`-P`) auto-publish the image's EXPOSE list.
        let mut ports: Vec<PortMap> = Vec::new();
        for raw in publish_raw {
            match flags::publish::parse_publish_spec(raw) {
                Ok(mut maps) => ports.append(&mut maps),
                Err(code) => return code,
            }
        }
        // WP-B2: `-P/--publish-all` — auto-publish every port the rootfs image
        // EXPOSEs (TCP), each bound on the default interface. The vz container
        // path hydrates the rootfs, so load its image config sidecar to read the
        // EXPOSE list. De-duplicated against explicit `-p` host ports so a port
        // named by both `-p` and EXPOSE is bound once.
        if publish_all {
            for pm in expose_port_maps(ref_name, &store) {
                if !ports.iter().any(|p| p.host == pm.host) {
                    ports.push(pm);
                }
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
            env_explicit,
            // RUNTIME flags persisted to spec.json; the native supervisor honors them.
            workdir: workdir.map(String::from),
            user: user.map(String::from),
            restart: restart.map(String::from),
            stop_signal: stop_signal.map(String::from),
            // WP-RESLIMITS: carry the parsed `--memory`/`--cpus` to spec.json so
            // the vz supervisor reads them back (the VM applies a hard mem/vcpu
            // cap — `vz_caps`). RUNTIME-ONLY, never keyed.
            limits,
            // WP-RC-FLAGS: the 11 run-config carry-fields (RUNTIME-ONLY, never
            // keyed). Persisted to spec.json + honored by the apply seam where the
            // native engine can; honest per-field note otherwise (see apply_cfg).
            hostname: rc.hostname.clone(),
            labels: rc.labels.clone(),
            cap_add: rc.cap_add.clone(),
            cap_drop: rc.cap_drop.clone(),
            privileged: rc.privileged,
            tty: rc.tty,
            init: rc.init,
            read_only: rc.read_only,
            oom_score_adj: rc.oom_score_adj,
            pids_limit: rc.pids_limit,
            shm_size: rc.shm_size,
            // WP-RUNFLAGS: `--name`/`--rm`/`--entrypoint` + `-v`/`--tmpfs` carry-
            // fields. Persisted to spec.json; honored on the native supervisor
            // path. RUNTIME-ONLY (never keyed). All-default ⇒ no-op.
            volumes: runflags.volumes.clone(),
            tmpfs: runflags.tmpfs.clone(),
            entrypoint: runflags.entrypoint.clone(),
            name: runflags.name.clone(),
            rm: runflags.rm,
            // WP-NET3: the vz container-networking carry-fields, off the C9 seam.
            // RUNTIME-ONLY, never keyed. `--network Some(..)` ⇒ the vz supervisor
            // (svz) create-or-opens the per-network registry, joins it, and
            // attaches the shared cross-process L2 switch (mesh NIC eth1). All
            // empty/None ⇒ the single-NAT-NIC path, byte-identical to before.
            network: runflags.network.clone(),
            network_alias: runflags.network_alias.clone(),
            add_host: runflags.add_host.clone(),
            dns: runflags.dns.clone(),
        };
        return match spawn_detached_engine(
            &spec,
            &store,
            healthcheck.as_ref(),
            EngineKind::Vz,
            Some(ref_name),
            &[],
        ) {
            Ok(handle) => claim_name_and_print(&handle, runflags.name.as_deref()),
            Err(e) => die_lightr(&e),
        };
    }

    if use_engine_path {
        // WP-DF-IMGCFG: run_engine honors the rootfs image config (CLI > image).
        // WP-IMG-ENVUSER: also consumes the image ENV + USER, with the CLI
        // `-e`/`--env-file` (`env_explicit`) and `-u`/`--user` overriding per
        // Docker precedence (image < CLI). `env_explicit` is only MOVED into the
        // native-memo path below, which this branch returns before — borrow is safe.
        return paths::run_engine(
            engine_kind,
            rootfs_ref,
            &store,
            &cwd,
            command,
            limits,
            workdir,
            &env_explicit,
            user,
            net_isolate,
            // WP-#92: `--read-only` / `--shm-size` reach the ns engine (enforced
            // there via ExecSpec). RUNTIME-ONLY; never part of the memo key.
            rc.read_only,
            rc.shm_size,
            // WP-#94: `--cap-drop` / `--cap-add` reach the ns engine (real Linux
            // capability enforcement via ExecSpec). native/vz never get here with
            // caps set — the engine-aware guard above honest-errors them first.
            // RUNTIME-ONLY; never part of the memo key.
            &rc.cap_drop,
            &rc.cap_add,
            // WP-#95: `--init` reaches the ns engine (a real PID-1 reaper inside the
            // new pid namespace). RUNTIME-ONLY; never part of the memo key.
            rc.init,
            // WP-#106: `--apparmor` reaches the ns engine (aa_change_onexec, applied
            // as the last pre-execv step). native/vz never get here with it set — the
            // engine-aware guard above honest-errors them first. RUNTIME-ONLY.
            rc.apparmor.as_deref(),
        );
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
        env_explicit,
        workdir: workdir.map(String::from),
        user: user.map(String::from),
        restart: restart.map(String::from),
        stop_signal: stop_signal.map(String::from),
        // WP-RC-FLAGS: the resolved 11 run-config flags (RUNTIME-ONLY).
        rc,
        // WP-RUNFLAGS: the resolved `-v`/`--tmpfs`/`--name`/`--rm`/`--entrypoint`.
        runflags,
    })
}

#[cfg(test)]
mod tests_net;
