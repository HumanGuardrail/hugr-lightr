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
    run_memoized_deep, run_memoized_with, run_vz_memoized, spawn_detached,
    spawn_detached_with_health, DeepMemoConfig, Mount, PortMap, RunSpec, StoreFile, VzMemoKey,
};
use lightr_store::Store;

use crate::exit::die_lightr;

use super::{parse_publish, RunJson};

// ── vz-memo path (the product's core moat) ───────────────────────────────────
// A `vz` container job with a rootfs that is NOT detached is MEMOIZABLE
// exactly like the native path: the 1st run boots the VM + captures
// {exit, stdout, stderr}; an identical 2nd run is a HIT that replays them
// from the Action Cache with NO VM boot.
pub(super) fn run_vz_memo(
    engine_kind: EngineKind,
    ref_name: &str,
    command: &[String],
    store: &Store,
    cwd: &std::path::Path,
    limits: ResourceLimits,
    json: bool,
) -> i32 {
    // 1. Resolve the rootfs ref → its content digest (the image identity for
    //    the key), exactly like a mount's key contribution in assemble_key:
    //    the ref's CURRENT root digest. A missing ref fails closed.
    let rootfs_digest = match store.ref_get(ref_name) {
        Ok(Some(rec)) => rec.root,
        Ok(None) => {
            eprintln!("lightr: run: rootfs ref not found: {ref_name}");
            return 1;
        }
        Err(e) => return die_lightr(&e),
    };

    // 2. The vz engine injects exactly this env into the guest (a fixed
    //    PATH; it does not inherit the host env). The memo key must use the
    //    SAME env so the key and the executed environment agree. Keep this
    //    in lock-step with VzEngine::run in crates/lightr-engine/src/lib.rs.
    // The memo key hashes the SAME PATH the vz engine injects into the guest
    // command — one source of truth (lightr_engine::GUEST_PATH, re-exported
    // from lightr_init) so the key can never drift from the actual env.
    let vz_env: Vec<(String, String)> =
        vec![("PATH".to_string(), lightr_engine::GUEST_PATH.to_string())];

    let key = VzMemoKey {
        command: command.to_vec(),
        rootfs_digest,
        env: vz_env,
    };

    // 3. Memoize. On a HIT the closure is never invoked (no VM boot). On a
    //    MISS the closure hydrates the rootfs, boots the VM via the engine,
    //    and reads the guest's stdout/stderr capture files back off the
    //    rootfs share (with a brief retry for virtiofs flush lag — the same
    //    pattern the engine uses for EXIT_FILE).
    let cwd_buf = cwd.to_path_buf();
    let outcome = run_vz_memoized(&key, store, || {
        // Hydrate the rootfs ref CoW into a temp dir for this boot.
        let tmp = tempfile::TempDir::new().map_err(lightr_core::LightrError::Io)?;
        lightr_index::hydrate(tmp.path(), store, ref_name)?;
        let rootfs_path = tmp.path().to_path_buf();

        let engine = engine_for(engine_kind)?;
        let spec = ExecSpec {
            cwd: &cwd_buf,
            command,
            rootfs: Some(rootfs_path.as_path()),
            limits,
            net: false,   // vz-memo path is non-detached + non-networked
            net_fd: None, // no mesh NIC on the memo path (ADR-0018)
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
        // Suppress the guest CONSOLE (kernel boot log + the exit marker) from
        // the host's stdout on a memo MISS: the command's real stdout/stderr
        // come from the capture files below, so the console is pure noise that
        // would otherwise prepend the boot log to the user's output (and make
        // a MISS look different from a HIT). The shim still taps the pipe for
        // the exit marker (the tap precedes the forward), so force-stop is
        // unaffected. Respect an explicit LIGHTR_VZ_CONSOLE (user debugging).
        if std::env::var_os("LIGHTR_VZ_CONSOLE").is_none() {
            // Safety: single-threaded here, before the engine spawns the VM.
            unsafe { std::env::set_var("LIGHTR_VZ_CONSOLE", "/dev/null") };
        }
        let exit = engine.run(&spec)?;

        // Read the guest's stdout/stderr capture files off the rootfs share.
        // PID1 fsyncs them BEFORE the console marker the engine waits on, so
        // they should be present; the retry loop covers virtiofs flush lag
        // (~30×100ms), mirroring the engine's EXIT_FILE read.
        //
        // Constants pinned to lightr_init::{STDOUT_FILE, STDERR_FILE}; kept
        // inline to avoid a new crate dependency (handler is a pure client
        // of the file channel, like the engine for CMD_FILE/EXIT_FILE).
        const STDOUT_FILE: &str = "/.lightr-stdout";
        const STDERR_FILE: &str = "/.lightr-stderr";
        let stdout_path = rootfs_path.join(STDOUT_FILE.trim_start_matches('/'));
        let stderr_path = rootfs_path.join(STDERR_FILE.trim_start_matches('/'));

        let read_capture = |path: &std::path::Path| -> Vec<u8> {
            for _ in 0..30 {
                if let Ok(bytes) = std::fs::read(path) {
                    return bytes;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            // A missing capture file is an empty stream (never an error): the
            // exit code is authoritative, and a non-zero exit isn't cached
            // anyway. Empty + exit==0 is a legitimately empty-output run.
            Vec::new()
        };
        let stdout = read_capture(&stdout_path);
        let stderr = read_capture(&stderr_path);

        // Keep the temp dir alive until after the files are read.
        drop(tmp);

        Ok((exit, stdout, stderr))
    });

    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => return die_lightr(&e),
    };

    // 4. Replay: write stdout then stderr raw (lossless), print the memo
    //    marker to stderr, exit = the (possibly replayed) exit code. Mirrors
    //    the native handler's streaming + marker.
    let hex = outcome.key.to_hex();
    let short = &hex[..16];
    let hit_str = if outcome.hit { "HIT" } else { "MISS" };
    eprintln!("lightr: memo {hit_str} key={short}");
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

// ── Engine path (non-memoized) ────────────────────────────────────────────────
pub(super) fn run_engine(
    engine_kind: EngineKind,
    rootfs_ref: Option<&str>,
    store: &Store,
    cwd: &std::path::Path,
    command: &[String],
    limits: ResourceLimits,
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

    let engine = match engine_for(engine_kind) {
        Ok(e) => e,
        Err(e) => return die_lightr(&e),
    };

    let spec = ExecSpec {
        cwd,
        command,
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
    /// every non-`-d` run (and when no `--health-cmd` is given), so the
    /// foreground path is byte-identical to before. The supervisor owns the
    /// watchdog; this only hands it the config.
    pub healthcheck: Option<Healthcheck>,
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
