//! Extracted path helpers for `lightr run`.
//!
//! Each helper is a verbatim extraction of one branch from the original
//! monolithic `run()` function. Signatures are the minimal set of already-
//! parsed inputs each branch needs; all behaviour is identical to the inlined
//! code (same branch conditions, same order, same exit codes).

use lightr_core::ResourceLimits;
use lightr_engine::{engine_for, EngineKind, ExecSpec, TmpfsMount, Ulimit};
use lightr_index;
use lightr_store::Store;

use crate::exit::die_lightr;

// The vz-memo path helper lives in `paths_vz.rs`, pulled in as a child module
// via `#[path]` to keep this file under the 400-line godfile cap, and re-exported
// so existing `paths::run_vz_memo(...)` callers are unchanged.
#[path = "paths_vz.rs"]
mod vz;
pub(super) use vz::run_vz_memo;

// ── Engine path (non-memoized) ────────────────────────────────────────────────
/// Run a hydrated image (`--rootfs <ref>`) through an engine, HONORING the
/// image's recorded config (WP-DF-IMGCFG): a `lightr build`-produced image
/// carries its config in the `.lightr-image.json` sidecar, and this path applies
/// the run-relevant fields with Docker precedence (CLI flag/arg > image default):
///
/// - **ENTRYPOINT + CMD** → the final argv is `effective_argv(cfg, command)`:
///   the image ENTRYPOINT is prepended, and a non-empty CLI `command` REPLACES
///   the image CMD (Docker last-wins). With no image config (an OCI/scratch base
///   without the sidecar) `cfg` is the default ⇒ argv == `command`, byte-identical
///   to before this WP (behaviour-preserved for config-less images).
/// - **WORKDIR** → the engine cwd-within-rootfs. The CLI `-w/--workdir` flag
///   overrides the image WORKDIR; absent both, the caller's `cwd` is used.
/// - **ENV** (WP-IMG-ENVUSER) → the image `Env` (KEY=VAL list) seeds the process
///   environment; the CLI `-e`/`--env-file` (`env_explicit`) OVERRIDES per key
///   (Docker precedence: image ENV < CLI). The merged set is carried on the
///   `ExecSpec` and applied by the engine at spawn (on top of the inherited env).
/// - **USER** (WP-IMG-ENVUSER) → the image `User` sets the process uid/gid; the
///   CLI `-u/--user` OVERRIDES it (Docker precedence: image USER < CLI). Carried
///   on the `ExecSpec`; the engine resolves name→uid + sets uid/gid (cfg unix).
///
/// All four are runtime-apply only: this is the NON-memoized engine path, so the
/// run memo key (computed pre-`ExecSpec` in `memo.rs`, for the native+no-rootfs
/// path) is structurally untouched by this WP. A config-less image + no CLI
/// env/user ⇒ empty env / `None` user ⇒ byte-identical to before (preserved).
/// (`--entrypoint` CLI override is the WP-RUNFLAGS stub's job, not owned here.)
#[allow(clippy::too_many_arguments)]
pub(super) fn run_engine(
    engine_kind: EngineKind,
    rootfs_ref: Option<&str>,
    store: &Store,
    cwd: &std::path::Path,
    command: &[String],
    limits: ResourceLimits,
    workdir: Option<&str>,
    // WP-IMG-ENVUSER: CLI `-e`/`--env-file` pairs — OVERRIDE the image ENV per key.
    env_explicit: &[(String, String)],
    // WP-IMG-ENVUSER: CLI `-u`/`--user` — OVERRIDES the image USER. `None` ⇒ image.
    user: Option<&str>,
    // WP-NET-ISO: `--net=none` ⇒ true: the ns engine creates a netns
    // (CLONE_NEWNET, loopback only). native ignores it; vz isolates via its VM.
    net_isolate: bool,
    // WP-#92: `--read-only` ⇒ the ns engine remounts the rootfs RO (fail-closed).
    read_only: bool,
    // WP-#92: `--shm-size` bytes ⇒ the ns engine sizes /dev/shm. `None` ⇒ 64 MiB.
    shm_size: Option<u64>,
    // WP-#94: `--cap-drop`/`--cap-add` ⇒ the ns engine drops the bounding set +
    // capsets the desired capability set (last step before exec). native/vz are
    // honest-errored at the handler, so these arrive empty there.
    cap_drop: &[String],
    cap_add: &[String],
    // WP-#95: `--init` ⇒ the ns engine runs a minimal PID-1 reaper inside the new
    // pid namespace (the workload becomes PID 2). native/vz ignore it (recorded-only
    // carry-slot). RUNTIME-ONLY; never part of the memo key.
    init: bool,
    // WP-#106: `--apparmor <profile>` ⇒ the ns engine applies the AppArmor profile
    // via aa_change_onexec as the last step before exec (fail-closed). native/vz are
    // honest-errored at the handler, so this arrives `None` there. RUNTIME-ONLY.
    apparmor: Option<&str>,
    // WP-#108: `--seccomp <path>` ⇒ the ns engine compiles the OCI profile to cBPF
    // (before pivot) and installs it right before exec (fail-closed). native/vz are
    // honest-errored at the handler, so this arrives `None` there. RUNTIME-ONLY.
    seccomp: Option<&str>,
    // `--add-host HOST:IP` ⇒ the ns engine appends `(ip, hostname)` lines to the
    // container's /etc/hosts before pivot. native is honest-errored at the handler.
    // RUNTIME-ONLY; never part of the memo key.
    add_host: &[(String, String)],
    // `--tmpfs DST[:size=..,mode=..]` ⇒ the ns engine mounts a tmpfs at each target
    // after /dev/shm. native/vz are honest-errored at the handler. RUNTIME-ONLY.
    tmpfs: &[TmpfsMount],
    // `--ulimit TYPE=SOFT[:HARD]` ⇒ per-process setrlimit caps. The native engine
    // (pre_exec setrlimit) + the ns engine (setrlimit in PID 1) apply them; vz is
    // honest-errored at the handler. RUNTIME-ONLY; never part of the memo key.
    ulimits: &[Ulimit],
    // `--oom-score-adj` ⇒ the ns engine writes /proc/self/oom_score_adj in PID 1
    // (fail-closed). The native engine has its OWN apply path (apply_cfg on the
    // memo path), so it ignores this ExecSpec field; vz's OOM tuning lives in the
    // guest. RUNTIME-ONLY; never part of the memo key.
    oom_score_adj: Option<i32>,
) -> i32 {
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
        if let Err(e) = lightr_index::hydrate(tmp.path(), store, ref_name) {
            return die_lightr(&e);
        }
        rootfs_path = Some(tmp.path().to_path_buf());
        rootfs_tmp = Some(tmp);
    } else {
        rootfs_tmp = None;
        rootfs_path = None;
    }

    // Load the hydrated image's config sidecar (WP-DF-IMGCFG). Absent (no rootfs,
    // or an image without the sidecar) ⇒ the DEFAULT config, so the argv/cwd below
    // are byte-identical to the pre-WP behaviour (no entrypoint, cmd == command).
    let cfg = match &rootfs_path {
        Some(p) => lightr_build::ImageConfig::load(p),
        None => lightr_build::ImageConfig::default(),
    };

    // ENTRYPOINT + CMD: prepend the image entrypoint; a non-empty CLI `command`
    // replaces the image CMD (Docker last-wins). Empty entrypoint + empty CLI
    // command + empty image CMD ⇒ empty argv ⇒ the engine's existing honest
    // "empty command" error (fail-closed, unchanged).
    let argv = lightr_build::effective_argv(&cfg, command);

    // WORKDIR: CLI `-w/--workdir` wins over the image WORKDIR (Docker precedence).
    // Absent both, keep the caller's cwd. Only meaningful WITH a rootfs (the path
    // is in-rootfs); a rootfs-less engine run keeps `cwd` as before.
    let run_cwd = resolve_run_cwd(workdir, cfg.workdir.as_deref(), rootfs_path.is_some(), cwd);

    // ENV: image `Env` seeds the process env, CLI `-e`/`--env-file` overrides per
    // key (Docker: image ENV < CLI). Empty image env + no CLI `-e` ⇒ empty merge
    // ⇒ the engine's apply is a no-op (behavior-preserving).
    let env = merge_image_env(&cfg.env, env_explicit);

    // USER: CLI `-u/--user` overrides the image `User` (Docker: image USER < CLI).
    // `None` everywhere ⇒ the engine runs as the current user (behavior-preserving).
    let eff_user = user.or(cfg.user.as_deref());

    let engine = match engine_for(engine_kind) {
        Ok(e) => e,
        Err(e) => return die_lightr(&e),
    };

    let spec = ExecSpec {
        cwd: &run_cwd,
        command: &argv,
        rootfs: rootfs_path.as_deref(),
        limits,
        net: false,   // synchronous CLI engine path; networked vz is detached (supervisor)
        net_isolate,  // WP-NET-ISO: `--net=none` ⇒ ns engine creates a netns (loopback only)
        net_fd: None, // mesh NIC is wired by the supervisor path (ADR-0018), not here
        net_mac: None,
        mounts: &[],
        env: &env,
        workdir: None,
        user: eff_user,
        hostname: None,
        // `--add-host`: the ns engine appends `(ip, hostname)` lines to the
        // container's /etc/hosts before pivot. Empty ⇒ unchanged.
        add_host,
        dns: &[],
        mesh_ip: None,
        // WP-#92: `--read-only` / `--shm-size` reach the ns engine here (the only
        // engine that enforces them). native ignores them (no rootfs to remount);
        // vz is its own VM. RUNTIME-ONLY — never part of the memo key.
        read_only,
        shm_size,
        // WP-#94: `--cap-drop`/`--cap-add` reach the ns engine here (the only engine
        // that enforces them; native/vz are honest-errored at the handler). The ns
        // engine applies them as the LAST step before exec. RUNTIME-ONLY — never
        // part of the memo key.
        cap_drop,
        cap_add,
        // WP-#95: `--init` reaches the ns engine here (the only engine that runs a
        // real PID-1 reaper; native/vz treat it as a recorded-only carry-slot).
        // RUNTIME-ONLY — never part of the memo key.
        init,
        // WP-#99: CRI-only carry-slots (join a pod netns / explicit cgroup leaf).
        // The CLI run path never sets them — defaults preserve today's behaviour.
        join_netns: None,
        cgroup_name: None,
        // WP-#102: exec-readiness signalling is a CRI-backend concern; the CLI run
        // path is synchronous (run() blocks) and never wires a pipe. Default None.
        exec_ready_fd: None,
        // WP-#106: `--apparmor <profile>` reaches the ns engine here (the only engine
        // that enforces it; native/vz are honest-errored at the handler). Applied via
        // aa_change_onexec as the last step before exec. RUNTIME-ONLY; never keyed.
        apparmor,
        // WP-#108: `--seccomp <path>` reaches the ns engine here (the only engine that
        // enforces it; native/vz are honest-errored at the handler). Compiled before
        // pivot, installed right before exec. RUNTIME-ONLY; never keyed.
        seccomp,
        // WP-#107: CRI volume mounts / DNS resolv.conf / hostname are CRI-backend
        // concerns (built in build_ns_plan from the sandbox/container config). The
        // CLI `lightr run` path never sets them — defaults preserve today's behaviour.
        bind_mounts: &[],
        resolv_conf: None,
        // `--tmpfs`: the ns engine mounts a tmpfs at each target after /dev/shm.
        // Empty ⇒ unchanged.
        tmpfs,
        // `--ulimit`: native (pre_exec) + ns (PID 1) apply per-process setrlimit
        // caps. Empty ⇒ unchanged.
        ulimits,
        // `--oom-score-adj`: the ns engine writes /proc/self/oom_score_adj in PID 1.
        // native ignores this field (it applies oom-score-adj via apply_cfg on the
        // memo path — no double-apply); vz's OOM tuning lives in the guest.
        oom_score_adj,
    };

    let code = match engine.run(&spec) {
        Ok(c) => c,
        Err(e) => return die_lightr(&e),
    };

    // Keep temp dir alive until after engine.run completes
    drop(rootfs_tmp);

    code
}

// `resolve_run_cwd` + `merge_image_env` (the pure `--rootfs` engine-path helpers)
// live in `paths_engine.rs` (pulled in via `#[path]` to keep this file under the
// 400-line godfile cap) and are re-exported so `paths::resolve_run_cwd` /
// `paths::merge_image_env` callers + tests are unchanged.
#[path = "paths_engine.rs"]
mod engine_helpers;
pub(super) use engine_helpers::{merge_image_env, resolve_run_cwd};

#[cfg(test)]
#[path = "paths_imgcfg_tests.rs"]
mod imgcfg_tests;

// Native memoized path — extracted to `paths_native_memo.rs` via `#[path]` to keep
// this file under the 400-line godfile cap; re-exported so callers are unchanged.
#[path = "paths_native_memo.rs"]
mod native_memo;
pub(super) use native_memo::{run_native_memo, NativeRun};
