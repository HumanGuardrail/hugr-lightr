//! WIN-PATH: the Windows named-pipe control-server helpers for the supervisor.
//!
//! Split out of `supervise.rs` (via `#[cfg(windows)] #[path] mod win;`) to keep
//! each file under the 400-line godfile cap. Validatable only on a real Windows
//! box; the unix CI gate never compiles this file (it is `#[cfg(windows)]` at the
//! `mod` site). Behaviour is a verbatim move — no logic change.

// WIN-PATH: blocking named-pipe accept loop for the control server. Each
// iteration creates one pipe instance, waits for a single client
// (ConnectNamedPipe), reads one newline-delimited JSON request, writes one
// JSON reply, then tears the instance down — the Windows analog of the unix
// accept-then-thread-per-connection model. Loops until `done` is set; the
// supervisor unblocks the final ConnectNamedPipe via `win_pipe_nudge`.
// Validatable only on a real Windows box.
pub(crate) fn win_pipe_server_loop(
    pipe_name: &str,
    child_pid: i32,
    done: &std::sync::atomic::AtomicBool,
) {
    use crate::run::paths::win_terminate;
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
pub(crate) fn win_pipe_nudge(pipe_name: &str) {
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
