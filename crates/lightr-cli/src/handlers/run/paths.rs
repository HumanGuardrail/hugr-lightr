//! Extracted path helpers for `lightr run`.
//!
//! Each helper is a verbatim extraction of one branch from the original
//! monolithic `run()` function. Signatures are the minimal set of already-
//! parsed inputs each branch needs; all behaviour is identical to the inlined
//! code (same branch conditions, same order, same exit codes).

use std::io::Write;

use lightr_core::ResourceLimits;
use lightr_engine::{engine_for, EngineKind, ExecSpec};
use lightr_index;
use lightr_run::healthcheck::Healthcheck;
use lightr_run::{
    run_memoized_deep, run_memoized_with, spawn_detached, spawn_detached_with_health,
    DeepMemoConfig, Mount, PortMap, RunSpec, StoreFile,
};
use lightr_store::Store;

use crate::exit::die_lightr;

use super::{parse_publish, RcConfig, RunJson};

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
///
/// Out of scope on THIS path (recorded by the build, NOT consumed here — flagged,
/// never silently honored): image ENV and USER. The engine `ExecSpec` carries
/// `env`/`user` fields but the native/ns/vz engines do not yet apply them
/// (engine-owned, outside this WP's files), so wiring them here would be a
/// recorded-but-ignored half-feature. The `--entrypoint` CLI override is gated
/// behind the WP-RUNFLAGS stub in `dispatch.rs` (not an owned file), so only the
/// image ENTRYPOINT is honored here (no in-scope CLI override exists yet).
pub(super) fn run_engine(
    engine_kind: EngineKind,
    rootfs_ref: Option<&str>,
    store: &Store,
    cwd: &std::path::Path,
    command: &[String],
    limits: ResourceLimits,
    workdir: Option<&str>,
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
        net_fd: None, // mesh NIC is wired by the supervisor path (ADR-0018), not here
        net_mac: None,
        mounts: &[],
        env: &[],
        workdir: None,
        user: None,
        hostname: None,
        add_host: &[],
        dns: &[],
        mesh_ip: None,
    };

    let code = match engine.run(&spec) {
        Ok(c) => c,
        Err(e) => return die_lightr(&e),
    };

    // Keep temp dir alive until after engine.run completes
    drop(rootfs_tmp);

    code
}

/// Resolve the engine cwd for a `--rootfs` run, honoring the image WORKDIR with
/// Docker precedence (WP-DF-IMGCFG): the CLI `-w/--workdir` flag wins over the
/// image's recorded WORKDIR; absent both, the caller's `fallback` cwd is used.
/// Only the CLI flag / image WORKDIR take effect WHEN a rootfs is present (the
/// recorded path is in-rootfs); a rootfs-less engine run always keeps `fallback`.
pub(super) fn resolve_run_cwd(
    cli_workdir: Option<&str>,
    image_workdir: Option<&str>,
    has_rootfs: bool,
    fallback: &std::path::Path,
) -> std::path::PathBuf {
    if !has_rootfs {
        return fallback.to_path_buf();
    }
    match (cli_workdir, image_workdir) {
        (Some(w), _) => std::path::PathBuf::from(w), // CLI > image (Docker precedence)
        (None, Some(w)) => std::path::PathBuf::from(w),
        (None, None) => fallback.to_path_buf(),
    }
}

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
    } = req;
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
        env_explicit,
        workdir,     // WP-RC-WORKDIR: honored as the child cwd (memo exec + supervisor).
        user,        // WP-RC-USER: honored as the child uid/gid (cfg unix; memo + supervisor).
        restart,     // WP-RC-RESTART: honored by the detached supervisor's re-spawn loop.
        stop_signal, // WP-RC-STOPSIGNAL: honored by `lightr stop`/restart-stop.
        // WP-RC-FLAGS: the resolved 11 run-config carry-fields. RUNTIME-ONLY
        // (never keyed). Honored by the apply seam (apply_cfg) on the native exec
        // + the detached supervisor; persisted to spec.json + shown by inspect
        // (labels/hostname). All-default ⇒ no-op (behavior-preserving).
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
    };

    // Detach path: spawn detached and print the run id. WP-RC-4: when a
    // healthcheck is configured, go through `spawn_detached_with_health` so the
    // supervisor probes it; with no healthcheck this is the same call shape as
    // before (`spawn_detached` == `_with_health(None)`), so the no-flags path is
    // behavior-preserving.
    if detach {
        let result = match healthcheck {
            Some(ref hc) => spawn_detached_with_health(&spec, store, Some(hc)),
            None => spawn_detached(&spec, store),
        };
        match result {
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
