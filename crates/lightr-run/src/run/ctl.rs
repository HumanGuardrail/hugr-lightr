//! Control transport path.
//! unix: a `.sock` unix-domain-socket path inside the run dir.
//! windows: a named pipe whose name is derived deterministically from the run
//!          id (the run dir's file name), so client and server agree without
//!          any extra shared state. A presence sentinel file in the run dir
//!          mirrors `.sock`'s "does the endpoint exist?" check.
//! JSON wire protocol is identical on both transports.

use std::path::PathBuf;

#[cfg(unix)]
pub(super) fn ctl_sock_path(dir: &std::path::Path) -> PathBuf {
    dir.join("ctl.sock")
}

// WIN-PATH: named-pipe address `\\.\pipe\lightr-<id>`. The id is the run dir's
// file name — the same identity the unix `.sock` lives under — so a client
// computes the identical pipe name from the same `dir`. Runtime-validatable
// only on a real Windows box.
#[cfg(windows)]
pub(super) fn ctl_pipe_name(dir: &std::path::Path) -> String {
    let id = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "default".to_string());
    format!(r"\\.\pipe\lightr-{id}")
}

// Windows sentinel mirroring `ctl.sock`'s existence semantics. The named pipe
// itself is not a filesystem object pollable via `Path::exists`, so the
// supervisor touches this file once the pipe server is listening and removes
// it on exit. `ps`/`stop` test this exactly like the unix `.sock` path.
#[cfg(windows)]
pub(super) fn ctl_sock_path(dir: &std::path::Path) -> PathBuf {
    dir.join("ctl.pipe.live")
}

#[cfg(unix)]
pub(super) fn send_ctl_op(dir: &std::path::Path, op: &str) -> Option<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let sock = ctl_sock_path(dir);
    if !sock.exists() {
        return None;
    }
    let mut stream = UnixStream::connect(&sock).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    stream.write_all(op.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

// WIN-PATH: named-pipe client. Opens `\\.\pipe\lightr-<id>` with CreateFileW
// (the pipe server is a BLOCKING PIPE_TYPE_BYTE / PIPE_WAIT pipe — see
// `supervise`), wraps the handle in a std File, and exchanges the SAME
// newline-delimited JSON request/response as the unix transport. The wire
// protocol is byte-identical; only the transport differs.
// Runtime-validatable only on a real Windows box.
#[cfg(windows)]
pub(super) fn send_ctl_op(dir: &std::path::Path, op: &str) -> Option<serde_json::Value> {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};

    // Mirror the unix `sock.exists()` guard: if the supervisor's live sentinel
    // is absent, there is no endpoint to talk to.
    let sentinel = ctl_sock_path(dir);
    if !sentinel.exists() {
        return None;
    }

    let name = ctl_pipe_name(dir);
    // Build a NUL-terminated wide string for CreateFileW.
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

    // dwShareMode=0, no security attrs, no extra flags (FILE_FLAGS_AND_ATTRIBUTES
    // is a u32 alias in windows-sys 0.59 — pass 0), no template handle.
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
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }

    // SAFETY: handle is a valid, owned pipe handle; File takes ownership and
    // closes it on drop.
    let file = unsafe { File::from_raw_handle(handle as *mut _) };
    // We need two independent halves (write the request, then buffered-read the
    // reply). try_clone duplicates the underlying handle.
    let mut writer = file.try_clone().ok()?;
    writer.write_all(op.as_bytes()).ok()?;
    writer.write_all(b"\n").ok()?;
    writer.flush().ok()?;

    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}
