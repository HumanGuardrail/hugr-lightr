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
use lightr_run::spawn_detached_engine;
use lightr_store::Store;

use crate::exit::die_lightr;

mod env;
mod flags;
mod helpers;
mod parse;
mod paths;
mod policy;
mod runflags;

// Value parsers (`--tmpfs`/`--ulimit`/`size=`) split to `parse.rs` (godfile cap).
use parse::{parse_tmpfs, parse_ulimits};

// Handler helpers split to `helpers.rs` (godfile cap). `claim_name_and_print` is
// also used by `paths.rs` via `super::` (re-exported here).
pub(super) use helpers::claim_name_and_print;

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
    // WP-B2: `-P/--publish-all` — auto-publish the image's EXPOSE list; `false` ⇒ none.
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
    // WP-RC-1: `-e`/`--env-file` → KEYED `env_explicit`; long `--env` = `env_keys` discovery.
    env_set: &[String],
    env_file: Option<&str>,
    // RUNTIME-ONLY docker-parity flags below (never keyed; `None` ⇒ today's behaviour):
    // WP-RC-WORKDIR `-w` (Docker WORKDIR; `None` ⇒ `dir`; CLI > image).
    workdir: Option<&str>,
    // WP-RC-USER `-u` (`None` ⇒ current user): native child uid/gid (cfg(unix)).
    user: Option<&str>,
    // WP-RC-RESTART `--restart` (`None` ⇒ `no`): supervisor re-spawn loop.
    restart: Option<&str>,
    // WP-RC-STOPSIGNAL `--stop-signal` (`None` ⇒ SIGTERM): `lightr stop`.
    stop_signal: Option<&str>,
    // WP-RC-4: healthcheck flags, WIRED — lowered to a Healthcheck (supervisor watchdog). Never keyed.
    health: &HealthFlags,
    // WP-RC-FLAGS: the 11 run-config flags (raw clap); resolved + lowered to RunSpec
    // carry-fields. RUNTIME-ONLY — never keyed; all-default ⇒ no-op.
    rc: RawRcFlags,
    // WP-RUNFLAGS: `-v`/`--tmpfs`/`--name`/`--rm`/`--entrypoint` (+ honest Phase-2 net
    // flags); resolved + lowered to carry-fields. RUNTIME-ONLY — never keyed.
    runflags: RawRunFlags,
) -> i32 {
    // WP-RC-FLAGS: parse `--label`/`--shm-size` (fail-closed: bad value ⇒ exit 2).
    let rc = match rc.resolve() {
        Ok(c) => c,
        Err(code) => return code,
    };

    // WP-#92: `--privileged` is honest-errored (exit 2) BEFORE provisioning — the
    // rootless ns engine can't enforce it (a silent no-op = false security). See
    // `policy::rc_privileged_policy`. WP-#94 `--cap-*` + WP-#95 `--init` are REAL on
    // `ns`; their engine-aware guard/note fires AFTER `engine_kind` is parsed below.
    if let Some(code) = policy::rc_privileged_policy(&rc) {
        return code;
    }

    // WP-RUNFLAGS: parse `-v`/`--entrypoint` + honest-error the networking flags
    // (fail-closed: bad value / Phase-2 flag ⇒ exit 2).
    let runflags = match runflags.resolve() {
        Ok(f) => f,
        Err(code) => return code,
    };
    // WP-RUNFLAGS: `--name`/`--rm` are detached-only (they need a run dir the
    // detached path creates) — honest exit 2 without `-d` (see policy fn).
    if let Some(code) = policy::detached_only_flags_policy(&runflags, detach) {
        return code;
    }
    // Parse engine kind — bad value ⇒ exit 2
    let engine_kind = match engine_str.parse::<EngineKind>() {
        Ok(k) => k,
        Err(e) => return die_lightr(&e),
    };

    // WP-#94/#106/#108: `--cap-*`/`--apparmor`/`--seccomp` enforce only on `ns`;
    // other engines are honest-errored here BEFORE provisioning (see policy fn).
    if let Some(code) = policy::engine_capability_policy(engine_kind, &rc) {
        return code;
    }

    // NOTE (`--user` on ns): a non-root `--user` is honest-errored by the ns ENGINE
    // itself (single-uid userns; subuid RANGE mapping tracked #115) — no handler guard.

    // `--tmpfs`/`--ulimit` on `vz` are honest-errored BEFORE provisioning (they'd
    // live inside the guest); ns/native handle them (see policy fn).
    if let Some(code) = policy::vz_mount_policy(engine_kind, &runflags) {
        return code;
    }

    // WP-#95: `--init` runs a real PID-1 reaper only on `ns`; elsewhere it is a
    // recorded-only carry-slot — say so honestly.
    policy::init_engine_note(rc.init, engine_kind);

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

    // WP-#90: a pids cap needs cgroup v2 `pids.max` (ns only); `--pids-limit
    // --engine vz` is honest-errored HERE, before the VM boots (see policy fn).
    if let Some(code) = policy::vz_pids_policy(engine_kind, &limits) {
        return code;
    }

    // Parse secrets/configs (F-309) — split NAME=REF.
    let secrets = match policy::resolve_store_files(secrets_raw, "secret") {
        Ok(v) => v,
        Err(code) => return code,
    };
    let configs = match policy::resolve_store_files(configs_raw, "config") {
        Ok(v) => v,
        Err(code) => return code,
    };

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
    policy::healthcheck_detach_note(healthcheck.is_some(), detach);

    // ── Networking Phase 1 policy (frozen, honest — enforce in this order) ────
    // These guards run BEFORE the engine-path early return below, so an
    // `--engine vz/ns -p ...` invocation hits the honest Phase-2 error rather
    // than silently dropping the published port.
    if let Some(code) = policy::publish_policy(publish_raw, detach, engine_kind, rootfs_ref) {
        return code;
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
    let mounts = match policy::resolve_mounts(mounts_raw) {
        Ok(v) => v,
        Err(code) => return code,
    };

    let cwd = std::path::PathBuf::from(dir);

    // ── DISPATCH ──────────────────────────────────────────────────────────────

    // ── vz-memo path (the product's core moat) ────────────────────────────────
    // A `vz`+rootfs job that is NOT detached is MEMOIZABLE like the native path: the
    // 1st run boots the VM + captures {exit, stdout, stderr}; an identical 2nd run is
    // a HIT replayed from the Action Cache with NO VM boot. Other cases fall through.
    if let (EngineKind::Vz, Some(ref_name), false) = (engine_kind, rootfs_ref, detach) {
        return paths::run_vz_memo(engine_kind, ref_name, command, &store, &cwd, limits, json);
    }

    // ── vz detached container path (WP-NET2) ──────────────────────────────────
    // A `vz` run WITH a rootfs that IS detached boots a Linux container in a microVM
    // under the supervisor, which forwards each published port to the guest's DHCP IP
    // (`-p` — the flagship Docker-parity case). `spawn_detached_engine` returns at once.
    if let (EngineKind::Vz, Some(ref_name), true) = (engine_kind, rootfs_ref, detach) {
        // WP-B2: range-aware `-p` (`8000-8002:8000-8002` ⇒ 3 maps; `127.0.0.1:H:C`
        // ⇒ loopback) + `-P/--publish-all` (auto-publish the image's EXPOSE list,
        // de-duplicated against explicit `-p` host ports). Fail-closed on a bad spec.
        let ports = match policy::resolve_detached_ports(publish_raw, publish_all, ref_name, &store)
        {
            Ok(v) => v,
            Err(code) => return code,
        };
        // Build the RunSpec persisted to spec.json (pure value builder — folds the
        // parsed inputs + resolved rc/runflags carry-fields; byte-identical).
        let spec = policy::build_detached_spec(
            cwd,
            command,
            env_keys,
            mounts,
            secrets,
            configs,
            ports,
            env_explicit,
            workdir,
            user,
            restart,
            stop_signal,
            limits,
            &rc,
            &runflags,
        );
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

    // `--ulimit TYPE=SOFT[:HARD]` ⇒ per-process setrlimit caps. Parsed ONCE here
    // (before the path split) because BOTH the engine path and the native memo path
    // apply it — enforceable natively, so never a silent no-op. Bad input ⇒ exit 2.
    let ulimits = match parse_ulimits(&runflags.ulimit) {
        Ok(v) => v,
        Err(code) => return code,
    };

    if use_engine_path {
        // `--add-host HOST:IP` ⇒ `(hostname, ip)` pairs for the ns engine's
        // /etc/hosts write (value-validated in `RawRunFlags::resolve`).
        let add_host_pairs = policy::resolve_add_host_pairs(&runflags);
        // `--tmpfs DST[:opts]` ⇒ the ns engine's tmpfs mounts. Minimal grammar:
        // `target` with an optional `:size=<bytes|N[kmg]>,mode=<octal>` suffix
        // (Docker's tmpfs option shape). Bad options are an honest exit 2.
        let tmpfs_mounts = match parse_tmpfs(&runflags.tmpfs) {
            Ok(v) => v,
            Err(code) => return code,
        };
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
            // WP-#108: `--seccomp` reaches the ns engine (cBPF filter install, applied
            // right before exec after apparmor). native/vz never get here with it set —
            // the engine-aware guard above honest-errors them first. RUNTIME-ONLY.
            rc.seccomp.as_deref(),
            // `--add-host` ⇒ the ns engine appends `(ip, hostname)` lines to the
            // container's /etc/hosts before pivot. native is honest-errored above.
            &add_host_pairs,
            // `--tmpfs` ⇒ the ns engine mounts a tmpfs at each target after /dev/shm.
            // native/vz are honest-errored above. RUNTIME-ONLY.
            &tmpfs_mounts,
            // `--ulimit` ⇒ the native engine (pre_exec setrlimit) + ns engine
            // (setrlimit in PID 1) apply per-process resource caps. vz is
            // honest-errored above. RUNTIME-ONLY.
            &ulimits,
            // `--oom-score-adj` ⇒ the ns engine writes /proc/self/oom_score_adj in
            // PID 1 (fail-closed). native applies it via apply_cfg on the memo path
            // (so this ExecSpec field is ignored there — no double-apply); vz's OOM
            // tuning lives in the guest. RUNTIME-ONLY; never part of the memo key.
            rc.oom_score_adj,
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
        // `--ulimit`: parsed per-process setrlimit caps, applied on the memo native
        // spawn via a pre_exec hook (enforceable natively ⇒ never silently dropped).
        ulimits,
    })
}

#[cfg(test)]
mod tests_net;
