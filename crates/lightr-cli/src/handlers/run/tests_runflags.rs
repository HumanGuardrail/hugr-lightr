//! WP-RUNFLAGS — `-v/--volume`, `--tmpfs`, `--name`, `--rm`, `--entrypoint` tests
//! (plus the relocated `mount_*` parse tests). The end-to-end tests exec the real
//! `run()` foreground native path under an isolated `LIGHTR_HOME` (serialized via
//! the crate ENV_LOCK, since LIGHTR_HOME is process-global) — matching the house
//! pattern in `tests.rs`.

use super::{parse_mount, run, HealthFlags, RawRcFlags, RawRunFlags};

// ── relocated mount_* parse tests (godfile-cap split from tests.rs) ──────────

#[test]
fn mount_parse_splits_on_first_colon() {
    let m = parse_mount("myref:some/target").expect("should parse");
    assert_eq!(m.ref_name, "myref");
    assert_eq!(m.target, "some/target");
}

#[test]
fn mount_parse_splits_on_first_colon_extra_colons() {
    let m = parse_mount("ref:sub:extra").expect("should parse");
    assert_eq!(m.ref_name, "ref");
    assert_eq!(m.target, "sub:extra");
}

#[test]
fn mount_rejects_absolute_target() {
    assert_eq!(parse_mount("ref:/abs/path").err().unwrap(), 2);
}

#[test]
fn mount_rejects_invalid_ref_name() {
    assert_eq!(parse_mount("INVALID:target").err().unwrap(), 2);
}

#[test]
fn mount_rejects_missing_colon() {
    assert_eq!(parse_mount("nocoton").err().unwrap(), 2);
}

#[test]
fn mount_accepts_relative_target() {
    let m = parse_mount("valid-ref:sub/dir").expect("should parse");
    assert_eq!(m.ref_name, "valid-ref");
    assert_eq!(m.target, "sub/dir");
}

// ── WP-RUNFLAGS end-to-end (foreground native path) ─────────────────────────

/// Run `command` in `dir` with the given `RawRunFlags` and defaults elsewhere.
/// Caller holds the ENV_LOCK + sets LIGHTR_HOME. Foreground native (no detach).
#[allow(clippy::too_many_arguments)]
fn run_fg(dir: &str, command: &[String], flags: RawRunFlags) -> i32 {
    run(
        dir,
        &[],
        &[],
        command,
        false, // json
        false, // explain
        false, // detach
        &[],   // publish
        &[],   // mounts
        "native",
        None,  // rootfs
        false, // deep_memo
        None,  // memory
        None,  // cpus
        &[],   // secrets
        &[],   // configs
        &[],   // env_set
        None,  // env_file
        None,  // workdir
        None,  // user
        None,  // restart
        None,  // stop_signal
        &HealthFlags::default(),
        RawRcFlags::default(),
        flags,
    )
}

/// `-v /host:/ctr` ⇒ the host file is visible at `cwd/<target>` in the run.
#[test]
fn volume_bind_makes_host_file_visible() {
    let _g = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");
    let host = tmp.path().join("host");
    std::fs::create_dir_all(&host).expect("mkdir host");
    std::fs::write(host.join("f.txt"), b"hi").expect("write host file");

    // `test -f data/f.txt` exits 0 iff the bind surfaced the host file.
    let flags = RawRunFlags {
        volume: vec![format!("{}:data", host.display())],
        ..RawRunFlags::default()
    };
    let code = run_fg(
        work.to_str().unwrap(),
        &[
            "sh".to_string(),
            "-c".to_string(),
            "test -f data/f.txt".to_string(),
        ],
        flags,
    );
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 0, "-v must surface the host file at the target");
}

/// `--tmpfs DST` ⇒ an empty writable dir at `cwd/DST` (a write into it succeeds).
#[test]
fn tmpfs_is_empty_and_writable() {
    let _g = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    let flags = RawRunFlags {
        tmpfs: vec!["scratch".to_string()],
        ..RawRunFlags::default()
    };
    let code = run_fg(
        work.to_str().unwrap(),
        &[
            "sh".to_string(),
            "-c".to_string(),
            "test -d scratch && touch scratch/w".to_string(),
        ],
        flags,
    );
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 0, "--tmpfs must be a writable empty dir");
}

/// `--entrypoint` prepends to the command: entrypoint `echo`, command `marker`
/// ⇒ the child is `echo marker` (exit 0). With a no-op entrypoint the exit proves
/// the prepend happened (a bogus entrypoint would fail to spawn).
#[test]
fn entrypoint_overrides_and_runs() {
    let _g = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    let flags = RawRunFlags {
        entrypoint: Some("echo".to_string()),
        ..RawRunFlags::default()
    };
    // command "hi" ⇒ argv "echo hi" ⇒ exit 0.
    let code = run_fg(work.to_str().unwrap(), &["hi".to_string()], flags);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 0, "--entrypoint must prepend + run");
}

/// `--name` without `-d` is an honest usage error (exit 2) — a foreground run has
/// no run dir to name.
#[test]
fn name_without_detach_is_exit_2() {
    let _g = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    let flags = RawRunFlags {
        name: Some("web".to_string()),
        ..RawRunFlags::default()
    };
    let code = run_fg(work.to_str().unwrap(), &["true".to_string()], flags);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 2, "--name without -d must exit 2");
}

/// `--rm` without `-d` is an honest usage error (exit 2).
#[test]
fn rm_without_detach_is_exit_2() {
    let _g = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    let flags = RawRunFlags {
        rm: true,
        ..RawRunFlags::default()
    };
    let code = run_fg(work.to_str().unwrap(), &["true".to_string()], flags);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 2, "--rm without -d must exit 2");
}

/// A network flag (`--network`) is an honest Phase-2 error (exit 2), NOT the
/// generic stub and NOT a silent drop.
#[test]
fn network_flag_is_honest_phase2() {
    let _g = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    let flags = RawRunFlags {
        network: Some("mynet".to_string()),
        ..RawRunFlags::default()
    };
    let code = run_fg(work.to_str().unwrap(), &["true".to_string()], flags);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 2, "--network must be an honest Phase-2 error");
}
