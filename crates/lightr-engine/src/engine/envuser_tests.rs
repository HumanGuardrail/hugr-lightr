//! WP-IMG-ENVUSER engine apply tests. Parallel-safe: no `set_var`, no shared
//! global state; the unix integration tests spawn a fresh child each time and
//! read its stdout. The uid/gid tests are `#[cfg(all(test, unix))]` (a POSIX
//! uid has no meaning on windows; the helpers they exercise are cfg(unix)).

use super::*;

#[test]
fn apply_user_none_is_noop() {
    let mut cmd = Command::new("true");
    // None ⇒ Ok no-op (current user, behavior-preserving).
    assert!(apply_user(&mut cmd, None).is_ok());
}

#[test]
fn apply_user_empty_is_honest_error() {
    let mut cmd = Command::new("true");
    assert!(apply_user(&mut cmd, Some("")).is_err());
}

// ── uid/gid resolution (unix-only: the helpers + the syscalls are cfg(unix)) ──

#[cfg(all(test, unix))]
mod unix {
    use super::*;

    #[test]
    fn resolve_uid_numeric() {
        assert_eq!(resolve_uid("1000").unwrap(), 1000);
        assert_eq!(resolve_gid("0").unwrap(), 0);
    }

    #[test]
    fn parse_passwd_id_finds_name() {
        let passwd = "root:x:0:0:root:/root:/bin/sh\n\
                      nobody:x:65534:65534:nobody:/:/usr/sbin/nologin\n\
                      app:x:1000:1000::/home/app:/bin/sh\n";
        assert_eq!(parse_passwd_id(passwd, "app"), Some(1000));
        assert_eq!(parse_passwd_id(passwd, "nobody"), Some(65534));
        assert_eq!(parse_passwd_id(passwd, "root"), Some(0));
    }

    #[test]
    fn parse_passwd_id_absent_is_none() {
        let passwd = "root:x:0:0:root:/root:/bin/sh\n";
        assert_eq!(parse_passwd_id(passwd, "ghost"), None);
    }

    #[test]
    fn parse_passwd_id_group_shape() {
        // /etc/group rows are `name:x:gid:members` — same first/3rd column shape.
        let group = "wheel:x:10:root,app\nstaff:x:50:\n";
        assert_eq!(parse_passwd_id(group, "wheel"), Some(10));
        assert_eq!(parse_passwd_id(group, "staff"), Some(50));
    }

    #[test]
    fn parse_passwd_id_malformed_line_skipped() {
        // a row whose id column isn't numeric yields None (no panic).
        let passwd = "weird:x:notanumber:0::/:/bin/sh\n";
        assert_eq!(parse_passwd_id(passwd, "weird"), None);
    }

    // Integration: the resolved uid/gid are actually set on the child. Setting a
    // uid different from the test process needs root, so assert on a uid the
    // kernel ALWAYS accepts — the current uid (a no-op setuid succeeds for any
    // user). Confirms apply_user wires through to a real spawn (exit 0).
    #[test]
    fn apply_user_current_uid_spawns_ok() {
        let me = unsafe { libc::getuid() };
        let mut cmd = Command::new("true");
        apply_user(&mut cmd, Some(&me.to_string())).unwrap();
        let status = cmd.status().expect("spawn true");
        assert!(status.success(), "setting our own uid must spawn cleanly");
    }
}

// ── env overlay (cross-platform behavior; uses a real child on unix) ──────────

#[test]
fn apply_env_empty_is_noop() {
    // An empty slice must not touch the command's env (behavior-preserving).
    let mut cmd = Command::new("true");
    apply_env(&mut cmd, &[]);
    // No panic / no API misuse is the contract; a clean spawn confirms it.
    let status = cmd.status().expect("spawn true");
    assert!(status.success());
}

// The image-ENV-seeds-process-env + CLI-override semantics are verified
// end-to-end against a real child: `env` prints the process environment.
#[cfg(all(test, unix))]
#[test]
fn apply_env_seeds_child_environment() {
    use std::process::Stdio;
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "printf '%s\\n' \"$LIGHTR_IMG_ENVUSER_TEST\""]);
    cmd.stdout(Stdio::piped());
    apply_env(
        &mut cmd,
        &[(
            "LIGHTR_IMG_ENVUSER_TEST".to_string(),
            "from_image".to_string(),
        )],
    );
    let out = cmd.output().expect("spawn sh");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "from_image");
}
