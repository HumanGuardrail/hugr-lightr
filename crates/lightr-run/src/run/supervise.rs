//! Process supervisor: supervise, win_pipe_server_loop, win_pipe_nudge.

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

    // Spawn child
    let mut child = std::process::Command::new(&spec.command[0])
        .args(&spec.command[1..])
        .current_dir(&cwd)
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

    // F-309: load an optional healthcheck persisted by spawn_detached_with_health.
    // The probe runs on the supervisor's poll loop at `interval_s`; its verdict
    // is written to `<run_dir>/health` for `ps`. Never part of the memo key (§0).
    let health_cfg = crate::healthcheck::load_for(dir)?;

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
            // Healthcheck probe round (interval-gated). A failing probe flips
            // <run_dir>/health to "unhealthy"; never aborts the loop.
            if let Some(ref hc) = health_cfg {
                if Instant::now() >= next_probe {
                    let verdict = crate::healthcheck::probe(hc, &health_cwd);
                    crate::healthcheck::write_state(dir, verdict);
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
            win_pipe_server_loop(&pipe_name_srv, child_pid, &done_srv);
            server_exited_srv.store(true, Ordering::SeqCst);
        });

        // Now that the server thread is up and will create the first pipe
        // instance, publish the sentinel so clients/`ps` see the endpoint.
        std::fs::write(&sentinel, b"live").map_err(LightrError::Io)?;

        // F-309: same interval-gated health probe as the unix path. WIN-PATH:
        // run_once uses `cmd /C`; runtime-validatable only on a real Windows box.
        let health_cwd = cwd.clone();
        let mut next_probe = std::time::Instant::now();

        // Main loop: poll child (identical cadence to the unix path).
        let exit_code = loop {
            if let Some(ref hc) = health_cfg {
                if std::time::Instant::now() >= next_probe {
                    let verdict = crate::healthcheck::probe(hc, &health_cwd);
                    crate::healthcheck::write_state(dir, verdict);
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
            win_pipe_nudge(&pipe_name);
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

// WIN-PATH: blocking named-pipe accept loop for the control server. Each
// iteration creates one pipe instance, waits for a single client
// (ConnectNamedPipe), reads one newline-delimited JSON request, writes one
// JSON reply, then tears the instance down — the Windows analog of the unix
// accept-then-thread-per-connection model. Loops until `done` is set; the
// supervisor unblocks the final ConnectNamedPipe via `win_pipe_nudge`.
// Validatable only on a real Windows box.
#[cfg(windows)]
pub(super) fn win_pipe_server_loop(
    pipe_name: &str,
    child_pid: i32,
    done: &std::sync::atomic::AtomicBool,
) {
    use super::paths::win_terminate;
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::io::FromRawHandle;
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();

    loop {
        if done.load(Ordering::SeqCst) {
            break;
        }

        // Create one blocking pipe instance.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            // Could not create the instance; back off briefly and retry unless
            // we are shutting down.
            if done.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        // Block until a client connects (or the nudge connection arrives).
        let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
        // ConnectNamedPipe returns 0 on failure; ERROR_PIPE_CONNECTED also means
        // a client is already present. Either way, if shutting down we bail.
        let _ = connected;

        if done.load(Ordering::SeqCst) {
            unsafe {
                DisconnectNamedPipe(handle);
                CloseHandle(handle);
            }
            break;
        }

        // Serve exactly one request/response on this instance using the SAME
        // newline-delimited JSON protocol as the unix transport.
        // SAFETY: handle is a valid owned pipe handle; File owns and closes it.
        let file = unsafe { File::from_raw_handle(handle as *mut _) };
        if let Ok(write_half) = file.try_clone() {
            let mut writer = write_half;
            let mut reader = BufReader::new(file);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let line = line.trim();
                if let Ok(req) = serde_json::from_str::<serde_json::Value>(line) {
                    let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
                    let reply: serde_json::Value = match op {
                        "status" => serde_json::json!({"status": "running"}),
                        "signal" => {
                            if let Some(sig) = req.get("sig").and_then(|v| v.as_i64()) {
                                // Map unix signal → forced TerminateProcess.
                                // Exit code follows the unix 128+sig convention.
                                let code = (128 + sig) as u32;
                                let ok = win_terminate(child_pid, code);
                                serde_json::json!({"ok": ok})
                            } else {
                                serde_json::json!({"ok": false})
                            }
                        }
                        _ => serde_json::json!({"error": "unknown op"}),
                    };
                    let mut reply_bytes = serde_json::to_vec(&reply).unwrap_or_default();
                    reply_bytes.push(b'\n');
                    let _ = writer.write_all(&reply_bytes);
                    let _ = writer.flush();
                }
            }
            // `reader` (and the underlying handle) and `writer` drop here,
            // flushing and closing the instance — disconnecting the client.
        }
    }
}

// WIN-PATH: unblock a pending ConnectNamedPipe by opening the pipe once and
// immediately dropping the connection. Used by the supervisor on child exit so
// the blocking server thread can observe `done` and terminate. Best-effort —
// failure is harmless (the next loop check still exits). Validatable only on a
// real Windows box.
#[cfg(windows)]
pub(super) fn win_pipe_nudge(pipe_name: &str) {
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};

    let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        // Own and immediately drop → closes the handle, completing the
        // server's ConnectNamedPipe so it can re-check `done`.
        let _f = unsafe { File::from_raw_handle(handle as *mut _) };
    }
}
