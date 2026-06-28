//! Extracted path helpers for `lightr run`.
//!
//! Each helper is a verbatim extraction of one branch from the original
//! monolithic `run()` function. Signatures are the minimal set of already-
//! parsed inputs each branch needs; all behaviour is identical to the inlined
//! code (same branch conditions, same order, same exit codes).

use std::io::Write;

use lightr_core::ResourceLimits;
use lightr_engine::{engine_for, EngineKind, ExecSpec, TmpfsMount};
use lightr_index;
use lightr_run::healthcheck::Healthcheck;
use lightr_run::{
    run_memoized_deep, run_memoized_with, spawn_detached, spawn_detached_with_health,
    DeepMemoConfig, Mount, PortMap, RunSpec, StoreFile,
};
use lightr_store::Store;

use crate::exit::die_lightr;

use super::{RcConfig, RunJson};

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

// ── Memoized path (native + no rootfs — unchanged R0/R1 behaviour) ────────────
/// All already-parsed inputs the native memoized path needs, bundled into one
/// struct (destructured at the top of [`run_native_memo`]) so the helper is a
/// single-argument call. The body below is identical to the inlined original.
pub(super) struct NativeRun<'a> {
    pub inputs: &'a [String],
    pub publish_raw: &'a [String],
    pub command: &'a [String],
    pub env_keys: &'a [String],
    pub mounts: Vec<Mount>,
    pub secrets: Vec<StoreFile>,
    pub configs: Vec<StoreFile>,
    pub cwd: std::path::PathBuf,
    pub detach: bool,
    pub store: &'a Store,
    pub explain: bool,
    pub json: bool,
    pub deep_memo: bool,
    pub limits: ResourceLimits,
    /// WP-RC-4: an optional healthcheck for the DETACHED native path. `None` for
    /// every non-`-d` run (and when no `--health-cmd` is given), so the foreground
    /// path is byte-identical to before; the supervisor owns the watchdog.
    pub healthcheck: Option<Healthcheck>,
    /// WP-RC-1 (R-KEY): user `-e`/`--env-file` env, resolved pairs — the ONLY env
    /// in the run memo key. Empty for no-`-e` runs (key/behaviour unchanged).
    pub env_explicit: Vec<(String, String)>,
    /// WP-RC-WORKDIR: `-w`/`--workdir` — honored as the child cwd. RUNTIME ONLY.
    pub workdir: Option<String>,
    /// WP-RC-USER: `-u`/`--user` — honored as the child uid/gid (cfg unix). RUNTIME ONLY.
    pub user: Option<String>,
    /// WP-RC-RESTART: `--restart` — honored by the detached supervisor's re-spawn
    /// loop. `None` ⇒ `no` (run once + exit). RUNTIME ONLY (never keyed).
    pub restart: Option<String>,
    /// WP-RC-STOPSIGNAL: `--stop-signal` — honored by `lightr stop`/restart-stop.
    /// `None` ⇒ SIGTERM. RUNTIME ONLY (never keyed).
    pub stop_signal: Option<String>,
    /// WP-RC-FLAGS: the resolved 11 run-config flags (hostname/labels/caps/
    /// privileged/tty/init/read-only/oom/pids/shm). Lowered into the RunSpec
    /// carry-fields + honored by the apply seam (or honest per-field note).
    /// RUNTIME ONLY (never keyed); all-default ⇒ no-op (behavior-preserving).
    pub rc: RcConfig,
    /// WP-RUNFLAGS: the resolved `-v/--volume` host binds, `--tmpfs` dirs,
    /// `--name`, `--rm`, `--entrypoint`. RUNTIME ONLY (never keyed); all-default ⇒
    /// no-op. Binds/tmpfs/entrypoint are honored on BOTH the synchronous memo exec
    /// and the detached supervisor; `--name`/`--rm` are detached-only (a foreground
    /// run has no run dir — guarded at the handler).
    pub runflags: super::runflags::RunFlags,
}

pub(super) fn run_native_memo(req: NativeRun) -> i32 {
    let NativeRun {
        inputs,
        publish_raw,
        command,
        env_keys,
        mounts,
        secrets,
        configs,
        cwd,
        detach,
        store,
        explain,
        json,
        deep_memo,
        limits,
        healthcheck,
        env_explicit,
        workdir,
        user,
        restart,
        stop_signal,
        rc,
        runflags,
    } = req;
    let input_paths: Vec<std::path::PathBuf> = if inputs.is_empty() {
        vec![cwd.clone()]
    } else {
        inputs.iter().map(std::path::PathBuf::from).collect()
    };

    // Parse published ports (Phase 1). Policy above already guaranteed this is
    // the native detached path when `publish_raw` is non-empty. Empty ⇒ no-op,
    // so the non-published path is byte-identical to before. WP-B2: consume the
    // range-aware, host-ip-carrying parser so `-p 8000-8002:8000-8002` yields 3
    // PortMaps and `-p 127.0.0.1:H:C` binds loopback. (`-P/--publish-all` has no
    // EXPOSE source on the native path — no rootfs image — so it is a no-op here;
    // it auto-publishes only on the rootfs-bearing vz container path.)
    let mut ports: Vec<PortMap> = Vec::new();
    for raw in publish_raw {
        match super::flags::publish::parse_publish_spec(raw) {
            Ok(mut maps) => ports.append(&mut maps),
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
        env_explicit,
        workdir,     // WP-RC-WORKDIR: honored as the child cwd (memo exec + supervisor).
        user,        // WP-RC-USER: honored as the child uid/gid (cfg unix; memo + supervisor).
        restart,     // WP-RC-RESTART: honored by the detached supervisor's re-spawn loop.
        stop_signal, // WP-RC-STOPSIGNAL: honored by `lightr stop`/restart-stop.
        limits,      // WP-RESLIMITS: caps → supervisor (RLIMIT_AS on Linux).
        // WP-RC-FLAGS: the resolved 11 run-config carry-fields. RUNTIME-ONLY
        // (never keyed). Honored by the apply seam (apply_cfg) on the native exec +
        // the detached supervisor; shown by inspect. All-default ⇒ no-op.
        hostname: rc.hostname,
        labels: rc.labels,
        cap_add: rc.cap_add,
        cap_drop: rc.cap_drop,
        privileged: rc.privileged,
        tty: rc.tty,
        init: rc.init,
        read_only: rc.read_only,
        oom_score_adj: rc.oom_score_adj,
        pids_limit: rc.pids_limit,
        shm_size: rc.shm_size,
        // WP-RUNFLAGS: host binds / tmpfs / entrypoint / name / rm carry-fields.
        // RUNTIME ONLY (never keyed). Binds + tmpfs force a memo MISS in
        // `run_memoized_with`; all-default ⇒ no-op (behavior-preserving).
        volumes: runflags.volumes,
        tmpfs: runflags.tmpfs,
        entrypoint: runflags.entrypoint,
        name: runflags.name.clone(),
        rm: runflags.rm,
        // WP-C9 seam: the vz container-networking carry-fields (network /
        // network_alias / add_host / dns). RUNTIME-ONLY, never keyed. The CLI
        // flag surface (`--network` etc.) is NET3's job; until then they default
        // to no-op, so this run path is byte-identical to before.
        ..Default::default()
    };

    // Detach path: spawn detached and print the run id. WP-RC-4: when a
    // healthcheck is configured, go through `spawn_detached_with_health` so the
    // supervisor probes it; with no healthcheck this is the same call shape as
    // before (`spawn_detached` == `_with_health(None)`), so the no-flags path is
    // behavior-preserving.
    if detach {
        // WP-RESLIMITS: validate caps BEFORE forking (honest sync error).
        if let Err(e) = lightr_run::limits::check_native_support(&limits) {
            return die_lightr(&e);
        }
        let result = match healthcheck {
            Some(ref hc) => spawn_detached_with_health(&spec, store, Some(hc)),
            None => spawn_detached(&spec, store),
        };
        match result {
            // WP-RUNFLAGS: claim `--name` (if any) for the spawned id, then print
            // it. `None` ⇒ just print the id (byte-identical to before).
            Ok(handle) => return super::claim_name_and_print(&handle, runflags.name.as_deref()),
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
        match run_memoized_deep(&spec, store, &DeepMemoConfig { enabled: true }) {
            Ok(o) => o,
            Err(e) => return die_lightr(&e),
        }
    } else {
        match run_memoized_with(&spec, store, &limits) {
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
