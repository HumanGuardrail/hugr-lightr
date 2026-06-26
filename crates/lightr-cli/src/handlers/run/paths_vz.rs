//! The vz-memo path helper (`run_vz_memo`), extracted verbatim from `paths.rs`
//! to keep that file under the 400-line godfile cap.

use std::io::Write;

use lightr_core::ResourceLimits;
use lightr_engine::{engine_for, EngineKind, ExecSpec};
use lightr_run::{run_vz_memoized, VzMemoKey};
use lightr_store::Store;

use crate::exit::die_lightr;

use super::super::RunJson;

// ── vz-memo path (the product's core moat) ───────────────────────────────────
// A non-detached `vz` rootfs run is MEMOIZABLE like the native path (see the
// module doc): 1st run boots the VM + captures {exit,stdout,stderr}; an
// identical 2nd run is a HIT replayed from the AC with NO VM boot.
pub(crate) fn run_vz_memo(
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
            net: false,         // vz-memo path is non-detached + non-networked
            net_isolate: false, // vz isolates via its VM; no netns flag needed
            net_fd: None,       // no mesh NIC on the memo path (ADR-0018)
            net_mac: None,
            mounts: &[],
            env: &[],
            workdir: None,
            user: None,
            hostname: None,
            add_host: &[],
            dns: &[],
            mesh_ip: None,
            // WP-#92: the vz-memo path does not enforce these (vz is its own VM);
            // the ns engine is where --read-only/--shm-size gain teeth.
            read_only: false,
            shm_size: None,
            // WP-#94: capability enforcement is the ns engine's job; the vz-memo
            // path is its own VM. Defaults (no cap changes).
            cap_drop: &[],
            cap_add: &[],
            init: false,
        };
        // Suppress the guest CONSOLE (boot log + exit marker) from the host's
        // stdout on a memo MISS: real stdout/stderr come from the capture files
        // below, so the console is noise that would prepend the boot log (and
        // make a MISS look unlike a HIT). The shim still taps the pipe for the
        // exit marker (tap precedes forward), so force-stop is unaffected.
        // Respect an explicit LIGHTR_VZ_CONSOLE (user debugging).
        if std::env::var_os("LIGHTR_VZ_CONSOLE").is_none() {
            // Safety: single-threaded here, before the engine spawns the VM.
            unsafe { std::env::set_var("LIGHTR_VZ_CONSOLE", "/dev/null") };
        }
        let exit = engine.run(&spec)?;

        // Read the guest's stdout/stderr capture files off the rootfs share.
        // PID1 fsyncs them BEFORE the console marker the engine waits on; the
        // retry loop covers virtiofs flush lag (~30×100ms), mirroring the
        // engine's EXIT_FILE read. Constants pinned to lightr_init::{STDOUT_FILE,
        // STDERR_FILE}, kept inline to avoid a new crate dependency.
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
