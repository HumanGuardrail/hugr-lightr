//! Filesystem path helpers: lightr_home, run_dir_for_id, new_run_id,
//! read_spec_on_disk, write_spec_json, read_pid_file, read_status_file,
//! parse_exit_code_from_status, pid_alive (unix + windows), win_terminate.

use lightr_core::{LightrError, Result};
use std::path::PathBuf;

use super::types::SpecOnDisk;

pub(super) fn lightr_home() -> PathBuf {
    if let Ok(h) = std::env::var("LIGHTR_HOME") {
        PathBuf::from(h)
    } else {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));
        home.join(".lightr")
    }
}

pub(super) fn run_dir_for_id(id: &str) -> PathBuf {
    lightr_home().join("run").join(id)
}

pub(super) fn new_run_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{nanos}-{pid}")
}

pub(super) fn read_spec_on_disk(dir: &std::path::Path) -> Result<SpecOnDisk> {
    let bytes = std::fs::read(dir.join("spec.json")).map_err(LightrError::Io)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| LightrError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

pub(super) fn write_spec_json(dir: &std::path::Path, spec: &SpecOnDisk) -> Result<()> {
    let bytes = serde_json::to_vec(spec).map_err(|e| LightrError::Io(std::io::Error::other(e)))?;
    std::fs::write(dir.join("spec.json"), &bytes).map_err(LightrError::Io)
}

pub(super) fn read_pid_file(dir: &std::path::Path) -> Option<i32> {
    std::fs::read_to_string(dir.join("pid"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

pub(super) fn read_status_file(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join("status"))
        .ok()
        .map(|s| s.trim().to_string())
}

pub(super) fn parse_exit_code_from_status(status: &str) -> Option<i32> {
    status
        .strip_prefix("exited ")
        .and_then(|s| s.parse::<i32>().ok())
}

#[cfg(unix)]
pub(super) fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

// WIN-PATH: liveness via OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) +
// GetExitCodeProcess; alive iff the process is still STILL_ACTIVE (259).
// Runtime-validatable only on a real Windows box.
#[cfg(windows)]
pub(super) fn pid_alive(pid: i32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle.is_null() {
            // Could not open: either gone or access-denied. Treat as not alive
            // (the supervisor owns the pid it spawned, so denial implies dead).
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        // STILL_ACTIVE is i32 259; GetExitCodeProcess writes a u32.
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

// ---------------------------------------------------------------------------
// Process termination — transport for SIGTERM/SIGKILL semantics.
// ---------------------------------------------------------------------------

// WIN-PATH: Windows has no signal model. SIGKILL maps to a forced
// TerminateProcess; SIGTERM is best-effort (no graceful-term equivalent — we
// force-terminate so `stop` makes progress). Graceful-term semantics differ
// from unix and are only validatable on a real Windows box.
// Returns true if a terminate was attempted successfully.
#[cfg(windows)]
pub(super) fn win_terminate(pid: i32, exit_code: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, exit_code);
        CloseHandle(handle);
        ok != 0
    }
}
