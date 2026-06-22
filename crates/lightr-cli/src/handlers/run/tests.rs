use super::{parse_publish, run, HealthFlags};

// ── parse_publish ───────────────────────────────────────────────────────

#[test]
fn publish_parses_host_container() {
    let p = parse_publish("8080:80").expect("should parse");
    assert_eq!(p.host, 8080);
    assert_eq!(p.container, 80);
}

#[test]
fn publish_accepts_explicit_tcp() {
    let p = parse_publish("39000:39001/tcp").expect("should parse");
    assert_eq!(p.host, 39000);
    assert_eq!(p.container, 39001);
}

#[test]
fn publish_rejects_udp_as_phase2() {
    let r = parse_publish("8080:80/udp");
    assert!(r.is_err());
    assert_eq!(r.err().unwrap(), 2);
}

#[test]
fn publish_rejects_missing_colon() {
    assert_eq!(parse_publish("8080").err().unwrap(), 2);
}

#[test]
fn publish_rejects_zero_port() {
    assert_eq!(parse_publish("0:80").err().unwrap(), 2);
    assert_eq!(parse_publish("80:0").err().unwrap(), 2);
}

#[test]
fn publish_rejects_out_of_range_and_nonnumeric() {
    // 70000 > u16::MAX ⇒ parse fails ⇒ Err(2).
    assert_eq!(parse_publish("70000:80").err().unwrap(), 2);
    assert_eq!(parse_publish("8080:abc").err().unwrap(), 2);
}

// ── policy guards (return 2 BEFORE any store/engine work) ─────────────────

#[test]
fn publish_without_detach_exits_2() {
    // -p given, detach=false ⇒ exit 2 (guard 1), before Store::open.
    let code = run(
        ".",
        &[],
        &[],
        &["true".to_string()],
        false, // json
        false, // explain
        false, // detach  ← NOT detached
        &["39000:39001".to_string()],
        false, // publish_all (WP-B2)
        &[],
        "native",
        None,
        false,
        None,
        None,
        &[],
        &[],
        &[],  // env_set (WP-RC-1)
        None, // env_file (WP-RC-1)
        None, // workdir (WP-RC-WORKDIR)
        None, // user (WP-RC-USER)
        None, // restart (WP-RC-RESTART)
        None, // stop_signal (WP-RC-STOPSIGNAL)
        &HealthFlags::default(),
        super::RawRcFlags::default(),  // WP-CLI-TRIO / RC-FLAGS
        super::RawRunFlags::default(), // WP-RUNFLAGS
    );
    assert_eq!(code, 2, "-p without -d must exit 2");
}

#[test]
fn publish_on_engine_path_exits_2() {
    // -p + -d but engine=vz ⇒ exit 2 (guard 2), before the engine early
    // return / any store work.
    let code = run(
        ".",
        &[],
        &[],
        &["true".to_string()],
        false,
        false,
        true, // detach
        &["39000:39001".to_string()],
        false, // publish_all (WP-B2)
        &[],
        "vz", // engine path ⇒ Phase 2
        None,
        false,
        None,
        None,
        &[],
        &[],
        &[],  // env_set (WP-RC-1)
        None, // env_file (WP-RC-1)
        None, // workdir (WP-RC-WORKDIR)
        None, // user (WP-RC-USER)
        None, // restart (WP-RC-RESTART)
        None, // stop_signal (WP-RC-STOPSIGNAL)
        &HealthFlags::default(),
        super::RawRcFlags::default(),  // WP-CLI-TRIO / RC-FLAGS
        super::RawRunFlags::default(), // WP-RUNFLAGS
    );
    assert_eq!(code, 2, "-p on the engine path must exit 2 (Phase 2)");
}

// ── HealthFlags::build (WP-RC-4) — moved to tests_health.rs (godfile cap) ──────

// ── parse_mount (existing) ────────────────────────────────────────────────

// The `mount_*` parse tests moved to `tests_runflags.rs` (godfile-cap split).

// ── WP-RC-1: `-e` is WIRED (no longer the WP-RUNFLAGS stub) ────────────────

/// A native run WITH `-e KEY=VAL` set actually RUNS (exit = the command's exit),
/// proving `-e`/`--env-file` were removed from the dispatch stub guard and flow
/// through to the keyed env_explicit channel. (The pre-WP-RC-1 guard returned
/// the WP-RUNFLAGS stub, exit 2, the instant `-e` was set.)
#[test]
fn dash_e_runs_not_stubbed() {
    let _env_guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    let code = run(
        work.to_str().unwrap(),
        &[],
        &[],
        &["true".to_string()],
        false, // json
        false, // explain
        false, // detach
        &[],   // publish
        false, // publish_all (WP-B2)
        &[],   // mounts
        "native",
        None,                     // rootfs
        false,                    // deep_memo
        None,                     // memory
        None,                     // cpus
        &[],                      // secrets
        &[],                      // configs
        &["FOO=bar".to_string()], // env_set (WP-RC-1) — must NOT be stubbed
        None,                     // env_file
        None,                     // workdir (WP-RC-WORKDIR)
        None,                     // user (WP-RC-USER)
        None,                     // restart (WP-RC-RESTART)
        None,                     // stop_signal (WP-RC-STOPSIGNAL)
        &HealthFlags::default(),
        super::RawRcFlags::default(),  // WP-CLI-TRIO / RC-FLAGS
        super::RawRunFlags::default(), // WP-RUNFLAGS
    );
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(
        code, 0,
        "a run with -e must execute the command (exit 0), not return the stub (exit 2)"
    );
}

// ── WP-RC-WORKDIR: `-w`/`--workdir` is WIRED (no longer the WP-RUNFLAGS stub) ──

/// A native run WITH `-w <sub>` set actually RUNS (exit 0) AND auto-creates the
/// workdir, proving `-w` was removed from the dispatch stub guard and flows
/// through to RunSpec.workdir → honored as the child cwd. (Pre-WP-RC-WORKDIR the
/// guard returned the WP-RUNFLAGS stub, exit 2, the instant `-w` was set.) The
/// command writes its CWD to a file under the run root; we assert it equals the
/// auto-created workdir — the end-to-end honor proof.
#[test]
fn dash_w_runs_not_stubbed_and_honored() {
    let _env_guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    // The workdir does NOT exist yet — the run must create it (Docker WORKDIR).
    let marker = work.join("pwd.out");
    // `sh -c 'pwd > <marker>'`: the child's pwd is captured to a file at the run
    // root, so we can compare it against the resolved workdir.
    let script = format!("pwd > {}", marker.display());

    let code = run(
        work.to_str().unwrap(),
        &[],
        &[],
        &["sh".to_string(), "-c".to_string(), script],
        false, // json
        false, // explain
        false, // detach
        &[],   // publish
        false, // publish_all (WP-B2)
        &[],   // mounts
        "native",
        None,           // rootfs
        false,          // deep_memo
        None,           // memory
        None,           // cpus
        &[],            // secrets
        &[],            // configs
        &[],            // env_set
        None,           // env_file
        Some("sub/wd"), // workdir (WP-RC-WORKDIR) — must NOT be stubbed
        None,           // user (WP-RC-USER)
        None,           // restart (WP-RC-RESTART)
        None,           // stop_signal (WP-RC-STOPSIGNAL)
        &HealthFlags::default(),
        super::RawRcFlags::default(),  // WP-CLI-TRIO / RC-FLAGS
        super::RawRunFlags::default(), // WP-RUNFLAGS
    );
    std::env::remove_var("LIGHTR_HOME");

    assert_eq!(
        code, 0,
        "a run with -w must execute the command (exit 0), not return the stub (exit 2)"
    );
    let expected = work.join("sub/wd");
    assert!(
        expected.is_dir(),
        "workdir must be auto-created (Docker WORKDIR semantics)"
    );
    let observed = std::fs::read_to_string(&marker).expect("pwd marker written");
    // Canonicalize both sides — macOS /var → /private/var symlink, etc.
    let observed = std::fs::canonicalize(observed.trim()).expect("canon observed");
    let expected = std::fs::canonicalize(&expected).expect("canon expected");
    assert_eq!(
        observed, expected,
        "the child must run with cwd == the resolved workdir"
    );
}

// ── WP-RC-USER: `-u`/`--user` is WIRED (no longer the WP-RUNFLAGS stub) ──────

/// A native run WITH `-u <current uid>` set actually RUNS (exit 0), proving
/// `-u` was removed from the dispatch stub guard and flows through to
/// RunSpec.user → honored as the child's uid (cfg(unix)). We use the CURRENT uid
/// (read via `id -u`) so the kernel needs NO privilege to set it — this is the
/// behavior-preserving honor path. (Pre-WP-RC-USER the guard returned the
/// WP-RUNFLAGS stub, exit 1, the instant `-u` was set.) cfg(unix) so the windows
/// gate — where `-u` is an honest error — never sees these bindings.
#[cfg(unix)]
#[test]
fn dash_u_current_uid_runs_not_stubbed() {
    let _env_guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    // Read THIS process's uid so setting it requires no privilege (the faithful
    // behavior-preserving path). `id -u` avoids a libc dependency for the test.
    let uid = String::from_utf8(
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .expect("id -u")
            .stdout,
    )
    .expect("uid utf8");
    let uid = uid.trim().to_string();

    let code = run(
        work.to_str().unwrap(),
        &[],
        &[],
        &["true".to_string()],
        false, // json
        false, // explain
        false, // detach
        &[],   // publish
        false, // publish_all (WP-B2)
        &[],   // mounts
        "native",
        None,       // rootfs
        false,      // deep_memo
        None,       // memory
        None,       // cpus
        &[],        // secrets
        &[],        // configs
        &[],        // env_set
        None,       // env_file
        None,       // workdir
        Some(&uid), // user (WP-RC-USER) — must NOT be stubbed
        None,       // restart (WP-RC-RESTART)
        None,       // stop_signal (WP-RC-STOPSIGNAL)
        &HealthFlags::default(),
        super::RawRcFlags::default(),  // WP-CLI-TRIO / RC-FLAGS
        super::RawRunFlags::default(), // WP-RUNFLAGS
    );
    std::env::remove_var("LIGHTR_HOME");

    assert_eq!(
        code, 0,
        "a run with -u <current uid> must execute (exit 0), not the stub (exit 1)"
    );
}

// ── WP-RC-STOPSIGNAL: `--stop-signal` is WIRED (never on the WP-RUNFLAGS stub) ──

/// A native run WITH `--stop-signal` set actually RUNS (exit 0), proving the flag
/// flows through to RunSpec.stop_signal (honored later by `lightr stop`) and is
/// NOT trapped by the dispatch stub guard. The signal value was validated at
/// dispatch; the handler is behavior-preserving for a non-detached run (the stop
/// path is exercised by the lightr-run stop tests).
#[test]
fn stop_signal_runs_not_stubbed() {
    let _env_guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    let code = run(
        work.to_str().unwrap(),
        &[],
        &[],
        &["true".to_string()],
        false, // json
        false, // explain
        false, // detach
        &[],   // publish
        false, // publish_all (WP-B2)
        &[],   // mounts
        "native",
        None,           // rootfs
        false,          // deep_memo
        None,           // memory
        None,           // cpus
        &[],            // secrets
        &[],            // configs
        &[],            // env_set
        None,           // env_file
        None,           // workdir
        None,           // user
        None,           // restart
        Some("SIGINT"), // stop_signal (WP-RC-STOPSIGNAL) — must NOT be stubbed
        &HealthFlags::default(),
        super::RawRcFlags::default(),  // WP-CLI-TRIO / RC-FLAGS
        super::RawRunFlags::default(), // WP-RUNFLAGS
    );
    std::env::remove_var("LIGHTR_HOME");

    assert_eq!(
        code, 0,
        "a run with --stop-signal must execute (exit 0), not return a stub"
    );
}
