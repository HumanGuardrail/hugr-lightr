//! stop — send SIGTERM then SIGKILL to a detached run.

use lightr_core::Result;

use super::ctl::{ctl_sock_path, send_ctl_op};
use super::paths::{parse_exit_code_from_status, pid_alive, read_pid_file, read_status_file};

pub fn stop(dir: &std::path::Path, grace_secs: u64) -> Result<i32> {
    use std::time::{Duration, Instant};

    let sock = ctl_sock_path(dir);

    if sock.exists() {
        // Try sending SIGTERM via ctl.sock
        send_ctl_op(dir, r#"{"op":"signal","sig":15}"#);
    } else if let Some(pid) = read_pid_file(dir) {
        // Direct kill
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        // WIN-PATH: no SIGTERM equivalent — best-effort forced terminate with
        // the unix 128+SIGTERM(15)=143 exit code. Graceful-term semantics
        // differ from unix; validatable only on a real Windows box.
        #[cfg(windows)]
        {
            super::paths::win_terminate(pid, 143);
        }
    }

    // Poll for grace_secs
    let deadline = Instant::now() + Duration::from_secs(grace_secs);
    loop {
        if Instant::now() >= deadline {
            break;
        }
        // Check if already exited
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            if let Some(code) = parse_exit_code_from_status(&status) {
                return Ok(code);
            }
        }
        // Check pid alive (pid_alive is implemented on unix and windows)
        if let Some(pid) = read_pid_file(dir) {
            if !pid_alive(pid) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Check again after grace
    let status = read_status_file(dir).unwrap_or_default();
    if status.starts_with("exited") {
        if let Some(code) = parse_exit_code_from_status(&status) {
            return Ok(code);
        }
    }

    // Still alive — SIGKILL
    if let Some(pid) = read_pid_file(dir) {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        // WIN-PATH: SIGKILL → forced TerminateProcess with the unix
        // 128+SIGKILL(9)=137 exit code. Validatable only on a real Windows box.
        #[cfg(windows)]
        {
            super::paths::win_terminate(pid, 137);
        }
    }

    // Wait a bit for status file update
    let kill_deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            if let Some(code) = parse_exit_code_from_status(&status) {
                return Ok(code);
            }
        }
        if std::time::Instant::now() >= kill_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(137)
}
