//! Process supervisor: `supervise` (+ the healthcheck watchdog). The Windows
//! named-pipe control-server helpers live in `supervise_win.rs` (mod `win`).

use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::PathBuf;

use super::ctl::ctl_sock_path;
use super::memo::validate_mount_target;
use super::paths::{lightr_home, read_spec_on_disk};
use super::svz::supervise_vz;

pub fn supervise(dir: &std::path::Path) -> Result<i32> {
    let spec = read_spec_on_disk(dir)?;
    let cwd = PathBuf::from(&spec.cwd);

    // Hydrate mounts (same law as run_memoized)
    // We need a store for hydration — open from LIGHTR_HOME
    let store_root = lightr_home().join("store");
    let store = Store::open(&store_root)?;

    // WP-NET2: a vz container run (engine "vz" + a rootfs ref) boots a Linux
    // microVM in this supervisor process and forwards each published port to the
    // guest's DHCP IP, instead of spawning a host child. Everything below is the
    // unchanged native path. Selected by the engine field written at spawn time.
    if spec.engine == "vz" && spec.rootfs_ref.is_some() {
        return supervise_vz(dir, &spec, &store);
    }

    for m in &spec.mounts {
        validate_mount_target(&m.target)?;
        let dest = cwd.join(&m.target);
        lightr_index::hydrate(&dest, &store, &m.ref_name)?;
    }

    // Open log files
    let stdout_log = std::fs::File::create(dir.join("stdout.log")).map_err(LightrError::Io)?;
    let stderr_log = std::fs::File::create(dir.join("stderr.log")).map_err(LightrError::Io)?;

    // WP-RC-WORKDIR: honor `-w`/`--workdir` (persisted to spec.json at spawn) as
    // the detached child's cwd (Docker WORKDIR), creating it if absent. `None` ⇒
    // `cwd` unchanged + no mkdir, so a plain `-d` run is byte-identical to before.
    // Mounts/healthcheck stay anchored at the run's `cwd` (the workspace root);
    // only the child process moves into the workdir.
    let run_cwd = super::spawn::resolve_workdir(&cwd, spec.workdir.as_deref())?;

    // Spawn child
    let mut child = std::process::Command::new(&spec.command[0])
        .args(&spec.command[1..])
        .current_dir(&run_cwd)
        // WP-DISC: explicit per-child env (compose service discovery
        // <PEER>_HOST/<PEER>_PORT + the service's own env), plumbed through
        // spec.json instead of the racy process-global set_var. Empty for a
        // plain `lightr run -d` (byte-identical to before).
        .envs(spec.env.iter().cloned())
        .stdout(std::process::Stdio::from(stdout_log))
        .stderr(std::process::Stdio::from(stderr_log))
        .spawn()
        .map_err(LightrError::Io)?;

    let child_pid = child.id() as i32;

    // Write pid file
    std::fs::write(dir.join("pid"), format!("{child_pid}")).map_err(LightrError::Io)?;

    // Write status = running
    std::fs::write(dir.join("status"), "running").map_err(LightrError::Io)?;

    // Networking Phase 1: publish each declared port by forwarding
    // 127.0.0.1:host → 127.0.0.1:container (where the child's server listens).
    // A bind failure is logged to stderr.log and skipped — it never kills the
    // run (a port clash on one publish must not take the whole service down).
    // The handles are held for the supervisor loop's lifetime; when the
    // supervisor exits (child gone / stop), they drop, the listeners close, and
    // the accept-loop + per-connection threads end. `_forwarders` is bound (not
    // `let _ =`) precisely so it is NOT dropped early.
    let mut _forwarders: Vec<crate::portforward::Forwarder> = Vec::new();
    if !spec.ports.is_empty() {
        for &(host_port, container_port) in &spec.ports {
            match crate::portforward::start(host_port, container_port) {
                Ok(fwd) => _forwarders.push(fwd),
                Err(e) => {
                    use std::io::Write as _;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join("stderr.log"))
                    {
                        let _ = writeln!(
                            f,
                            "lightr: publish 127.0.0.1:{host_port} -> 127.0.0.1:{container_port} failed: {e}"
                        );
                    }
                }
            }
        }
    }

    // F-309 / WP-RC-4: load an optional healthcheck persisted by
    // spawn_detached_with_health. The probe runs on the supervisor's poll loop
    // at `interval_s`, each probe capped at `timeout_s`; the verdict folds into
    // a HealthState machine (starting → healthy/unhealthy, with a start-period
    // grace and a FailingStreak) and the status is written to `<run_dir>/health`
    // for `ps`. Never part of the memo key (§0).
    let health_cfg = crate::healthcheck::load_for(dir)?;

    // The watchdog's monotonic launch instant + state machine. A probe is "in
    // the start period" while `launched.elapsed() < start_period_s`. The machine
    // begins in Starting; we write that immediately so `ps` shows "starting"
    // before the first probe round completes.
    let health_launched = std::time::Instant::now();
    let mut health_state = crate::healthcheck::HealthState::default();
    if health_cfg.is_some() {
        crate::healthcheck::write_state(dir, health_state.status);
    }

    // The control transport is cfg-split below; the JSON wire protocol
    // (newline-delimited `{"op":...}` request → `{...}` reply) is identical.

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::time::{Duration, Instant};

        // Bind ctl.sock
        let sock_path = ctl_sock_path(dir);
        let listener = UnixListener::bind(&sock_path).map_err(LightrError::Io)?;
        listener.set_nonblocking(true).map_err(LightrError::Io)?;

        // Healthcheck cwd is the run's cwd; first probe runs immediately so `ps`
        // surfaces a verdict without waiting a full interval.
        let health_cwd = cwd.clone();
        let mut next_probe = Instant::now();

        // Main loop: serve ctl.sock + poll child + (if configured) probe health
        let exit_code = loop {
            // Healthcheck probe round (interval-gated). One round = one
            // probe_once verdict folded into the HealthState machine; a failure
            // only flips <run_dir>/health to "unhealthy" after retries+1
            // consecutive post-grace failures. Never aborts the loop.
            if let Some(ref hc) = health_cfg {
                if Instant::now() >= next_probe {
                    let passed = crate::healthcheck::probe_once(hc, &health_cwd);
                    let in_start = health_launched.elapsed().as_secs() < hc.start_period_s;
                    health_state.record(passed, in_start, hc.retries);
                    crate::healthcheck::write_state(dir, health_state.status);
                    next_probe = Instant::now() + Duration::from_secs(hc.interval_s.max(1));
                }
            }

            // Poll child
            if let Some(status) = child.try_wait().map_err(LightrError::Io)? {
                use std::os::unix::process::ExitStatusExt;
                let code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                break code;
            }

            // Accept ctl connections (non-blocking)
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(1))).ok();
                    stream.set_write_timeout(Some(Duration::from_secs(1))).ok();
                    let mut reader = BufReader::new(&stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() {
                        let line = line.trim();
                        if let Ok(req) = serde_json::from_str::<serde_json::Value>(line) {
                            let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
                            let reply: serde_json::Value = match op {
                                "status" => serde_json::json!({"status": "running"}),
                                "signal" => {
                                    if let Some(sig) = req.get("sig").and_then(|v| v.as_i64()) {
                                        unsafe {
                                            libc::kill(child_pid, sig as libc::c_int);
                                        }
                                        serde_json::json!({"ok": true})
                                    } else {
                                        serde_json::json!({"ok": false})
                                    }
                                }
                                _ => serde_json::json!({"error": "unknown op"}),
                            };
                            let mut reply_bytes = serde_json::to_vec(&reply).unwrap_or_default();
                            reply_bytes.push(b'\n');
                            let mut w = &stream;
                            let _ = w.write_all(&reply_bytes);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {}
            }

            std::thread::sleep(Duration::from_millis(100));
        };

        // Write final status
        std::fs::write(dir.join("status"), format!("exited {exit_code}"))
            .map_err(LightrError::Io)?;

        // Remove ctl.sock
        let _ = std::fs::remove_file(&sock_path);

        Ok(exit_code)
    }

    #[cfg(windows)] // WIN-PATH: named-pipe control server, identical JSON wire protocol.
    {
        use super::ctl::ctl_pipe_name;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        // Presence sentinel mirroring unix `ctl.sock` existence semantics: write
        // it once the pipe server is listening, remove it on exit. `ps`/`stop`
        // poll this file exactly like the unix `.sock`.
        let sentinel = ctl_sock_path(dir);
        let pipe_name = ctl_pipe_name(dir);

        let done = Arc::new(AtomicBool::new(false));
        let done_srv = Arc::clone(&done);
        // `server_exited`: retry nudge until server thread leaves loop (avoids
        // the race where a single nudge misses, leaving join() to hang forever).
        let server_exited = Arc::new(AtomicBool::new(false));
        let server_exited_srv = Arc::clone(&server_exited);
        let pipe_name_srv = pipe_name.clone();

        // Pipe-server thread: handles same ops as unix listener; `signal` →
        // TerminateProcess with unix 128+sig convention (143/137).
        let server = std::thread::spawn(move || {
            win::win_pipe_server_loop(&pipe_name_srv, child_pid, &done_srv);
            server_exited_srv.store(true, Ordering::SeqCst);
        });

        // Now that the server thread is up and will create the first pipe
        // instance, publish the sentinel so clients/`ps` see the endpoint.
        std::fs::write(&sentinel, b"live").map_err(LightrError::Io)?;

        // F-309 / WP-RC-4: same interval-gated health probe + state machine as
        // the unix path. WIN-PATH: run_once uses `cmd /C`; runtime-validatable
        // only on a real Windows box.
        let health_cwd = cwd.clone();
        let mut next_probe = std::time::Instant::now();

        // Main loop: poll child (identical cadence to the unix path).
        let exit_code = loop {
            if let Some(ref hc) = health_cfg {
                if std::time::Instant::now() >= next_probe {
                    let passed = crate::healthcheck::probe_once(hc, &health_cwd);
                    let in_start = health_launched.elapsed().as_secs() < hc.start_period_s;
                    health_state.record(passed, in_start, hc.retries);
                    crate::healthcheck::write_state(dir, health_state.status);
                    next_probe =
                        std::time::Instant::now() + Duration::from_secs(hc.interval_s.max(1));
                }
            }
            if let Some(status) = child.try_wait().map_err(LightrError::Io)? {
                // No signal() on Windows; ExitStatus::code is authoritative.
                break status.code().unwrap_or(1);
            }
            std::thread::sleep(Duration::from_millis(100));
        };

        // Tell the server thread to stop, then keep nudging until it actually
        // leaves its loop. Retrying closes the race where a single nudge lands
        // before the server has created a pipe instance — after which the next
        // ConnectNamedPipe would block forever and join() would hang.
        done.store(true, Ordering::SeqCst);
        while !server_exited.load(Ordering::SeqCst) {
            win::win_pipe_nudge(&pipe_name);
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = server.join();

        // Write final status
        std::fs::write(dir.join("status"), format!("exited {exit_code}"))
            .map_err(LightrError::Io)?;

        // Remove the presence sentinel (the named pipe itself is freed when its
        // handles close in the server thread).
        let _ = std::fs::remove_file(&sentinel);

        Ok(exit_code)
    }
}

// WIN-PATH: the Windows named-pipe control-server helpers
// (`win_pipe_server_loop` / `win_pipe_nudge`) live in `supervise_win.rs`, pulled
// in via `#[path]` to keep this file under the 400-line godfile cap. The module
// is `#[cfg(windows)]`, so the unix CI gate never compiles it; the windows
// branch above calls `win::win_pipe_server_loop` / `win::win_pipe_nudge`.
#[cfg(windows)]
#[path = "supervise_win.rs"]
mod win;
