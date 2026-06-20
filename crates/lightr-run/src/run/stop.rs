//! stop — send the configured stop signal (default SIGTERM) then SIGKILL to a
//! detached run.

use lightr_core::Result;

use super::ctl::{ctl_sock_path, send_ctl_op};
use super::paths::{
    parse_exit_code_from_status, pid_alive, read_pid_file, read_spec_on_disk, read_status_file,
};

/// WP-RC-STOPSIGNAL: resolve a user `--stop-signal` spec to its raw signal
/// number. Accepts a bare non-negative decimal (`"9"`, `"15"`) or one of the
/// five POSIX-portable signal names stable across macOS and Linux,
/// case-insensitive with an optional `SIG` prefix: HUP=1, INT=2, QUIT=3,
/// KILL=9, TERM=15. An unrecognised spec yields `None` (caller falls back to
/// SIGTERM — never a silent wrong signal). Mirrors the `kill` verb's parser
/// (handlers/kill.rs); the two share the portable-name contract.
fn parse_stop_signal(spec: &str) -> Option<i32> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Ok(n) = spec.parse::<i32>() {
        return if n >= 0 { Some(n) } else { None };
    }
    let upper = spec.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    match name {
        "HUP" => Some(1),
        "INT" => Some(2),
        "QUIT" => Some(3),
        "KILL" => Some(9),
        "TERM" => Some(15),
        _ => None,
    }
}

/// WP-RC-STOPSIGNAL: the graceful stop signal for the run in `dir` — its
/// configured `--stop-signal` (from spec.json) if present and parseable, else
/// SIGTERM (15), today's byte-identical behaviour. An unreadable spec or an
/// unparseable signal falls back to SIGTERM rather than failing the stop.
fn configured_stop_signal(dir: &std::path::Path) -> i32 {
    read_spec_on_disk(dir)
        .ok()
        .and_then(|s| s.stop_signal)
        .and_then(|s| parse_stop_signal(&s))
        .unwrap_or(15)
}

pub fn stop(dir: &std::path::Path, grace_secs: u64) -> Result<i32> {
    use std::time::{Duration, Instant};

    // WP-RC-RESTART: `stop` is the EXPLICIT-stop path (the `lightr stop` verb and
    // `rm -f` route through here). Write the stop marker BEFORE killing the child
    // so the supervisor's re-spawn loop sees it on the child's exit and does NOT
    // restart — covering the direct-kill branch below, which never round-trips
    // through the supervisor's ctl handler. A no-restart run is unaffected.
    super::respawn::write_stop_marker(dir);

    // WP-RC-STOPSIGNAL: the graceful signal is the run's configured
    // `--stop-signal`, defaulting to SIGTERM (15) when unset — so a run with no
    // `--stop-signal` is byte-identical to before.
    let stop_sig = configured_stop_signal(dir);

    let sock = ctl_sock_path(dir);

    if sock.exists() {
        // Send the configured stop signal via ctl.sock (the supervisor relays it
        // to the child). Default SIGTERM when unset.
        send_ctl_op(dir, &format!(r#"{{"op":"signal","sig":{stop_sig}}}"#));
    } else if let Some(pid) = read_pid_file(dir) {
        // Direct kill with the configured stop signal.
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, stop_sig as libc::c_int);
        }
        // WIN-PATH: no signal model — best-effort forced terminate with the unix
        // 128+signal exit code for the configured stop signal (143 for the
        // SIGTERM default). Graceful-term semantics differ from unix; validatable
        // only on a real Windows box.
        #[cfg(windows)]
        {
            super::paths::win_terminate(pid, (128 + stop_sig) as u32);
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

#[cfg(test)]
mod tests {
    use super::{configured_stop_signal, parse_stop_signal};
    use crate::run::paths::write_spec_json;
    use crate::run::types::SpecOnDisk;

    #[test]
    fn parse_numeric_and_portable_names() {
        assert_eq!(parse_stop_signal("9"), Some(9));
        assert_eq!(parse_stop_signal("15"), Some(15));
        assert_eq!(parse_stop_signal("0"), Some(0));
        assert_eq!(parse_stop_signal("HUP"), Some(1));
        assert_eq!(parse_stop_signal("INT"), Some(2));
        assert_eq!(parse_stop_signal("QUIT"), Some(3));
        assert_eq!(parse_stop_signal("KILL"), Some(9));
        assert_eq!(parse_stop_signal("TERM"), Some(15));
        // case-insensitive, optional SIG prefix, trimmed.
        assert_eq!(parse_stop_signal("sigterm"), Some(15));
        assert_eq!(parse_stop_signal("  Hup "), Some(1));
    }

    #[test]
    fn parse_rejects_garbage_and_negatives() {
        assert_eq!(parse_stop_signal(""), None);
        assert_eq!(parse_stop_signal("nope"), None);
        assert_eq!(parse_stop_signal("-1"), None);
        // platform-specific names are not mapped by name.
        assert_eq!(parse_stop_signal("USR1"), None);
    }

    fn write_spec(dir: &std::path::Path, stop_signal: Option<&str>) {
        let spec = SpecOnDisk {
            stop_signal: stop_signal.map(String::from),
            ..Default::default()
        };
        write_spec_json(dir, &spec).unwrap();
    }

    #[test]
    fn configured_defaults_to_sigterm_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        write_spec(dir.path(), None);
        // No `--stop-signal` ⇒ SIGTERM (15), byte-identical to before.
        assert_eq!(configured_stop_signal(dir.path()), 15);
    }

    #[test]
    fn configured_honors_the_set_signal() {
        let dir = tempfile::tempdir().unwrap();
        write_spec(dir.path(), Some("SIGINT"));
        assert_eq!(configured_stop_signal(dir.path()), 2);
        let dir2 = tempfile::tempdir().unwrap();
        write_spec(dir2.path(), Some("9"));
        assert_eq!(configured_stop_signal(dir2.path()), 9);
    }

    #[test]
    fn configured_falls_back_to_sigterm_on_unreadable_or_unparseable() {
        // No spec.json at all ⇒ SIGTERM (never fail the stop).
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(configured_stop_signal(dir.path()), 15);
        // A spec with an unparseable signal ⇒ SIGTERM fallback.
        let dir2 = tempfile::tempdir().unwrap();
        write_spec(dir2.path(), Some("nonsense"));
        assert_eq!(configured_stop_signal(dir2.path()), 15);
    }
}
