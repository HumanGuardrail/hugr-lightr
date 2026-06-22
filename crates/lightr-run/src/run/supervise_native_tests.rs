//! WP-RC-RESTART — integration tests for the native supervisor's re-spawn loop.
//!
//! Each test drives `supervise()` directly in a thread (a unit test cannot use
//! `spawn_detached`, which needs the real `lightr` binary via current_exe). The
//! child is a short-lived `/bin/sh` snippet that APPENDS a byte to a counter
//! file each spawn, so the test counts re-spawns by polling the file — never by
//! a fixed sleep that races CI. Parallel-safe: `LIGHTR_HOME` is serialised by
//! the shared `ENV_LOCK`, each test uses its own tempdir.
#![cfg(all(test, unix))]

use crate::run::paths::{read_status_file, write_spec_json};
use crate::run::respawn;
use crate::run::stop::stop;
use crate::run::supervise::supervise;
use crate::run::types::SpecOnDisk;
use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = crate::run::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("LIGHTR_HOME", home.path());
    (home, guard)
}

/// Create a run dir with a spec.json carrying `command` + `restart`, then launch
/// `supervise()` in a thread. Returns (run_dir, join_handle).
fn start(
    home: &std::path::Path,
    cwd: &std::path::Path,
    command: Vec<String>,
    restart_policy: Option<&str>,
) -> (std::path::PathBuf, std::thread::JoinHandle<i32>) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Keep the id SHORT: the run dir holds `ctl.sock`, whose absolute path must
    // fit in the unix-domain `SUN_LEN` (~104 bytes). Each test has its own home
    // tempdir, so a short non-unique-across-homes suffix is fine.
    let id = format!("{}", nanos % 1_000_000_000);
    let run_dir = home.join("run").join(&id);
    fs::create_dir_all(&run_dir).unwrap();

    // `supervise()` opens the store under LIGHTR_HOME/store; create it so the
    // (mountless) supervisor opens cleanly instead of erroring out.
    lightr_store::Store::open(home.join("store")).expect("store open");

    let spec = SpecOnDisk {
        cwd: cwd.to_string_lossy().into_owned(),
        command,
        created_at_unix: nanos / 1_000_000_000,
        engine: "native".to_string(),
        restart: restart_policy.map(|s| s.to_string()),
        ..Default::default()
    };
    write_spec_json(&run_dir, &spec).unwrap();

    let rd = run_dir.clone();
    let t = std::thread::spawn(move || supervise(&rd).expect("supervise"));
    (run_dir, t)
}

/// Poll `f` until it returns true or `deadline_ms` elapses; returns the final
/// verdict. Generous deadline + small step — never a fixed race-prone sleep.
fn poll_until(deadline_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    f()
}

/// Count of bytes the child appended to the counter file = number of spawns.
fn spawn_count(counter: &std::path::Path) -> usize {
    fs::read(counter).map(|b| b.len()).unwrap_or(0)
}

/// A `/bin/sh -c` snippet that appends one byte to `counter` then exits `code`.
fn counting_child(counter: &std::path::Path, code: i32) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf x >> '{}'; exit {code}", counter.display()),
    ]
}

// ── `no` (default): the child runs EXACTLY once ─────────────────────────────
#[test]
fn no_policy_runs_once() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let counter = tmp.path().join("count");

    let (run_dir, t) = start(home.path(), tmp.path(), counting_child(&counter, 0), None);
    let code = t.join().expect("supervisor join");

    assert_eq!(code, 0, "no-policy child exits 0 once");
    assert_eq!(
        spawn_count(&counter),
        1,
        "`no` must run the child exactly once"
    );
    assert_eq!(
        read_status_file(&run_dir).as_deref(),
        Some("exited 0"),
        "final status is the single exit"
    );
}

// ── `always`: the child is re-spawned after it exits ────────────────────────
#[test]
fn always_respawns() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let counter = tmp.path().join("count");

    // Each child exits 0 immediately; `always` must keep re-spawning.
    let (run_dir, t) = start(
        home.path(),
        tmp.path(),
        counting_child(&counter, 0),
        Some("always"),
    );

    // Poll until we observe at least 3 spawns (proves the loop re-spawns).
    let got = poll_until(15_000, || spawn_count(&counter) >= 3);
    assert!(
        got,
        "`always` must re-spawn (saw {} spawns)",
        spawn_count(&counter)
    );

    // Explicit stop disables further restart; the supervisor then exits.
    respawn::write_stop_marker(&run_dir);
    let _ = stop(&run_dir, 2);
    let _ = t.join();
}

// ── `on-failure:max`: re-spawn on nonzero up to max, then stop ──────────────
#[test]
fn on_failure_respawns_up_to_max_then_stops() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let counter = tmp.path().join("count");

    // Child always fails (exit 1); max 2 retries ⇒ 1 initial + 2 = 3 spawns,
    // then the supervisor gives up and exits with the last failure code.
    let (run_dir, t) = start(
        home.path(),
        tmp.path(),
        counting_child(&counter, 1),
        Some("on-failure:2"),
    );

    let code = t.join().expect("supervisor join");
    assert_eq!(code, 1, "final exit is the last child's failure code");
    assert_eq!(
        spawn_count(&counter),
        3,
        "on-failure:2 must spawn exactly 3 times (1 initial + 2 retries)"
    );
    assert_eq!(read_status_file(&run_dir).as_deref(), Some("exited 1"));
}

// ── `on-failure` does NOT restart a clean (exit 0) child ────────────────────
#[test]
fn on_failure_does_not_restart_on_success() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let counter = tmp.path().join("count");

    let (_run_dir, t) = start(
        home.path(),
        tmp.path(),
        counting_child(&counter, 0),
        Some("on-failure:5"),
    );
    let code = t.join().expect("supervisor join");
    assert_eq!(code, 0);
    assert_eq!(
        spawn_count(&counter),
        1,
        "a clean exit must NOT trigger on-failure restart"
    );
}

// ── explicit stop disables restart (no re-spawn after stop) ─────────────────
#[test]
fn explicit_stop_disables_restart() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let counter = tmp.path().join("count");

    // A long-lived child under `always`: it stays up so we can stop it cleanly
    // and prove the loop does not re-spawn after the explicit stop.
    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf x >> '{}'; sleep 10", counter.display()),
    ];
    let (run_dir, t) = start(home.path(), tmp.path(), cmd, Some("always"));

    // Wait until the first child is up (counter has 1 byte + ctl endpoint live).
    let up = poll_until(10_000, || {
        spawn_count(&counter) >= 1 && crate::run::ctl::ctl_sock_path(&run_dir).exists()
    });
    assert!(up, "first child must come up");

    // Explicit stop (writes the marker + SIGTERMs via ctl). The supervisor must
    // NOT re-spawn → counter stays at 1 and the thread returns.
    let _ = stop(&run_dir, 3);
    let done = poll_until(10_000, || {
        read_status_file(&run_dir)
            .map(|s| s.starts_with("exited"))
            .unwrap_or(false)
    });
    assert!(done, "supervisor must terminate after an explicit stop");

    // Give the loop a beat: if it (wrongly) re-spawned, the counter would grow.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        spawn_count(&counter),
        1,
        "an explicit stop must NOT trigger a re-spawn"
    );
    let _ = t.join();
}

// ── WP-HYG (#71): stop reaps the WHOLE process tree, not just the child ──────

/// `kill(pid, 0)` liveness probe — local so it can poll a GRANDCHILD pid the
/// supervisor never tracked.
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// THE LEAK PROOF (#71). A `sh -c` child backgrounds a long-lived `sleep`
/// GRANDCHILD, records its pid, then `wait`s (so the `sh` stays up as the
/// supervised child). A naive single-process kill of the `sh` would orphan the
/// `sleep` to PPID 1 (the historical leak — 106 stranded `nc`). With the
/// process-group fix, `stop` signals the whole group ⇒ the grandchild is gone.
#[test]
fn stop_reaps_the_whole_process_group() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("grandchild.pid");

    // Background a sleep grandchild, publish its pid, then wait so the parent sh
    // stays alive — proving the kill must reach deeper than the immediate child.
    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("sleep 1000 & echo $! > '{}'; wait", pidfile.display()),
    ];
    let (run_dir, t) = start(home.path(), tmp.path(), cmd, None);

    let up = poll_until(10_000, || {
        crate::run::ctl::ctl_sock_path(&run_dir).exists() && pidfile.exists()
    });
    assert!(up, "child must come up and publish the grandchild pid");

    let grandchild = fs::read_to_string(&pidfile)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .expect("grandchild pid");
    assert!(alive(grandchild), "grandchild must be alive before stop");

    // The group SIGTERM (then SIGKILL) must reach the grandchild, not just `sh`.
    let _ = stop(&run_dir, 5);

    let gone = poll_until(10_000, || !alive(grandchild));
    assert!(
        gone,
        "LEAK: grandchild (pid {grandchild}) survived stop — the tree was not \
         reaped (bug #71)"
    );
    let _ = t.join();
}

// ── WP-RESLIMITS: the supervisor applies the persisted caps at spawn ─────────

/// Build a run dir with a spec.json carrying resource caps, then run `supervise()`
/// SYNCHRONOUSLY (no thread) and return its `Result`. Used to assert the
/// apply-at-spawn honest boundary without racing a background loop.
fn run_with_limits(
    home: &std::path::Path,
    cwd: &std::path::Path,
    command: Vec<String>,
    mem: Option<u64>,
    cpu: Option<u64>,
) -> lightr_core::Result<()> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let id = format!("rl{}", nanos % 1_000_000_000);
    let run_dir = home.join("run").join(&id);
    fs::create_dir_all(&run_dir).unwrap();
    lightr_store::Store::open(home.join("store")).expect("store open");

    let spec = SpecOnDisk {
        cwd: cwd.to_string_lossy().into_owned(),
        command,
        created_at_unix: nanos / 1_000_000_000,
        engine: "native".to_string(),
        mem_limit_bytes: mem,
        cpu_limit_millis: cpu,
        ..Default::default()
    };
    write_spec_json(&run_dir, &spec).unwrap();
    supervise(&run_dir).map(|_| ())
}

/// `None`/`None` (unlimited) ⇒ the supervisor spawns the child unchanged and it
/// runs to completion (behavior-preserving: a no-caps detached run is identical
/// to before).
#[test]
fn supervisor_unlimited_runs_unchanged() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let counter = tmp.path().join("count");
    let r = run_with_limits(
        home.path(),
        tmp.path(),
        counting_child(&counter, 0),
        None,
        None,
    );
    assert!(r.is_ok(), "unlimited supervise must succeed: {r:?}");
    assert_eq!(spawn_count(&counter), 1, "child runs exactly once");
}

/// A cpu *share* is NOT enforceable on the native engine (RLIMIT_CPU is total
/// cpu-seconds, not a share) ⇒ the supervisor FAILS the spawn with an honest
/// error rather than silently dropping the cap. Cross-platform on unix.
#[test]
fn supervisor_cpu_share_is_honest_err() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let counter = tmp.path().join("count");
    let r = run_with_limits(
        home.path(),
        tmp.path(),
        counting_child(&counter, 0),
        None,
        Some(500),
    );
    assert!(
        r.is_err(),
        "a cpu share must be an honest supervise error, not a silent drop"
    );
}

/// A memory cap installs the RLIMIT_AS hook on Linux ⇒ the child spawns and runs
/// (value plumbed, NOT a live OOM). Off Linux a native memory cap is an honest
/// error (macOS ignores RLIMIT_AS). Either way the cap is NEVER silently dropped.
#[test]
fn supervisor_memory_cap_plumbed() {
    let (home, _g) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let counter = tmp.path().join("count");
    let r = run_with_limits(
        home.path(),
        tmp.path(),
        counting_child(&counter, 0),
        Some(256 * 1024 * 1024),
        None,
    );
    #[cfg(target_os = "linux")]
    {
        assert!(r.is_ok(), "Linux memory cap plumbs + runs: {r:?}");
        assert_eq!(spawn_count(&counter), 1);
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert!(
            r.is_err(),
            "off-Linux native memory cap is an honest err (no silent drop)"
        );
    }
}
