//! Unit tests for the run-instance lifecycle primitives. Parallel-safe: every
//! test injects its OWN private tempdir as `home` (atomic counter + nanos unique)
//! and never mutates process-global state — matching CI's multi-threaded
//! `cargo test --workspace`.

use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A private tempdir used as the lifecycle `home` root, removed on drop. The
/// atomic counter + nanos guarantee a unique dir even under concurrent tests.
struct TmpHome {
    dir: PathBuf,
}

impl TmpHome {
    fn new(tag: &str) -> Self {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("lightr-life-{tag}-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        TmpHome { dir }
    }
    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TmpHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Materialize a run dir `<home>/run/<id>` with a minimal valid `spec.json`
/// (optionally carrying a registry `name`). Returns the run dir.
fn make_run_dir(home: &Path, id: &str, name: Option<&str>) -> PathBuf {
    let dir = home.join("run").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut spec = super::super::types::SpecOnDisk {
        cwd: home.to_string_lossy().into_owned(),
        command: vec!["true".to_string()],
        detached: true,
        ..Default::default()
    };
    spec.name = name.map(|s| s.to_string());
    super::super::paths::write_spec_json(&dir, &spec).unwrap();
    dir
}

/// Write the `exited <code>` terminal status the supervisor writes on child exit.
fn mark_exited(dir: &Path, code: i32) {
    std::fs::write(dir.join("status"), format!("exited {code}")).unwrap();
}

/// Fake a RUNNING run WITHOUT a real supervisor: `is_running` requires the ctl
/// endpoint file to exist AND the recorded pid to be alive. We touch the ctl
/// sentinel and record OUR OWN pid (guaranteed alive for the test's duration),
/// so the running-vs-stopped guards exercise the true detection path.
fn mark_running(dir: &Path) {
    std::fs::write(super::ctl_sock_path(dir), b"live").unwrap();
    std::fs::write(dir.join("pid"), format!("{}", std::process::id())).unwrap();
}

// ── run_status ─────────────────────────────────────────────────────────────

#[test]
fn status_missing_run_is_not_found() {
    let h = TmpHome::new("status-missing");
    let err = run_status(h.path(), "nope").unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}

#[test]
fn status_fresh_dir_is_unknown() {
    let h = TmpHome::new("status-unknown");
    make_run_dir(h.path(), "id-u", None);
    assert_eq!(run_status(h.path(), "id-u").unwrap(), RunStatus::Unknown);
}

#[test]
fn status_exited_reports_code() {
    let h = TmpHome::new("status-exited");
    let dir = make_run_dir(h.path(), "id-e", None);
    mark_exited(&dir, 7);
    assert_eq!(run_status(h.path(), "id-e").unwrap(), RunStatus::Exited(7));
}

#[test]
fn status_running_is_running() {
    let h = TmpHome::new("status-running");
    let dir = make_run_dir(h.path(), "id-r", None);
    mark_running(&dir);
    assert_eq!(run_status(h.path(), "id-r").unwrap(), RunStatus::Running);
}

// ── remove_run ─────────────────────────────────────────────────────────────

#[test]
fn remove_stopped_run_deletes_dir() {
    let h = TmpHome::new("rm-stopped");
    let dir = make_run_dir(h.path(), "id-rm", None);
    mark_exited(&dir, 0);
    remove_run(h.path(), "id-rm", false).unwrap();
    assert!(!dir.exists());
}

#[test]
fn remove_stopped_run_releases_name() {
    let h = TmpHome::new("rm-name");
    let dir = make_run_dir(h.path(), "id-named", Some("web"));
    super::super::registry::claim(h.path(), "web", "id-named").unwrap();
    mark_exited(&dir, 0);
    // Pre: the name resolves.
    assert_eq!(
        super::super::registry::resolve(h.path(), "web").unwrap(),
        "id-named"
    );
    remove_run(h.path(), "id-named", false).unwrap();
    // Post: dir gone AND the name freed (no longer resolves).
    assert!(!dir.exists());
    let err = super::super::registry::resolve(h.path(), "web").unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}

#[test]
fn remove_running_run_without_force_is_refused() {
    let h = TmpHome::new("rm-running-noforce");
    let dir = make_run_dir(h.path(), "id-run", None);
    mark_running(&dir);
    let err = remove_run(h.path(), "id-run", false).unwrap_err();
    match err {
        LightrError::InvalidRef(m) => assert!(m.contains("running"), "{m}"),
        other => panic!("expected InvalidRef, got {other:?}"),
    }
    // The dir must be UNTOUCHED on the refusal.
    assert!(dir.exists());
}

#[test]
fn remove_missing_run_is_not_found() {
    let h = TmpHome::new("rm-missing");
    let err = remove_run(h.path(), "ghost", false).unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}

// ── respawn_run ────────────────────────────────────────────────────────────

#[test]
fn respawn_running_run_is_refused() {
    let h = TmpHome::new("respawn-running");
    let dir = make_run_dir(h.path(), "id-up", None);
    mark_running(&dir);
    let err = respawn_run(h.path(), "id-up").unwrap_err();
    match err {
        LightrError::InvalidRef(m) => assert!(m.contains("already running"), "{m}"),
        other => panic!("expected InvalidRef, got {other:?}"),
    }
}

#[test]
fn respawn_missing_run_is_not_found() {
    let h = TmpHome::new("respawn-missing");
    let err = respawn_run(h.path(), "ghost").unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}

#[test]
fn respawn_clears_stale_exit_status() {
    // A stopped run carries a terminal `exited <code>`; respawn must clear it so
    // a subsequent `wait_run` does not read the PREVIOUS exit code. We don't
    // assert the supervisor actually boots (no real binary under test), only the
    // primitive's own pre-launch contract: stale status is gone before launch.
    //
    // launch_supervisor execs `current_exe() __supervise <dir>`; under the test
    // harness that's the test binary, which ignores those args and exits — it
    // never writes a NEW status, so after the call the status file stays absent.
    let h = TmpHome::new("respawn-clears");
    let dir = make_run_dir(h.path(), "id-clear", None);
    mark_exited(&dir, 42);
    assert!(dir.join("status").exists());
    respawn_run(h.path(), "id-clear").unwrap();
    assert!(
        !dir.join("status").exists(),
        "stale exit status must be cleared on respawn"
    );
}

// ── signal_run ─────────────────────────────────────────────────────────────

#[test]
fn signal_non_running_run_is_refused() {
    let h = TmpHome::new("signal-stopped");
    let dir = make_run_dir(h.path(), "id-sig", None);
    mark_exited(&dir, 0);
    let err = signal_run(h.path(), "id-sig", 15).unwrap_err();
    match err {
        LightrError::InvalidRef(m) => assert!(m.contains("not running"), "{m}"),
        other => panic!("expected InvalidRef, got {other:?}"),
    }
}

#[test]
fn signal_missing_run_is_not_found() {
    let h = TmpHome::new("signal-missing");
    let err = signal_run(h.path(), "ghost", 15).unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}

// ── wait_run ───────────────────────────────────────────────────────────────

#[test]
fn wait_already_exited_returns_code_immediately() {
    let h = TmpHome::new("wait-exited");
    let dir = make_run_dir(h.path(), "id-w", None);
    mark_exited(&dir, 3);
    assert_eq!(wait_run(h.path(), "id-w").unwrap(), 3);
}

#[test]
fn wait_missing_run_is_not_found() {
    let h = TmpHome::new("wait-missing");
    let err = wait_run(h.path(), "ghost").unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}

#[test]
fn wait_vanished_supervisor_fails_closed() {
    // Not running (no ctl endpoint) and no parseable exit code: wait_run must
    // fail closed rather than block forever.
    let h = TmpHome::new("wait-vanished");
    make_run_dir(h.path(), "id-v", None);
    let err = wait_run(h.path(), "id-v").unwrap_err();
    match err {
        LightrError::InvalidRef(m) => assert!(m.contains("not running"), "{m}"),
        other => panic!("expected InvalidRef, got {other:?}"),
    }
}

// ── list_stopped_runs (WP-D container prune) ─────────────────────────────────

#[test]
fn list_stopped_runs_empty_home_is_empty() {
    let h = TmpHome::new("prune-empty");
    assert!(list_stopped_runs(h.path()).unwrap().is_empty());
}

#[test]
fn list_stopped_runs_returns_only_exited() {
    // 2 exited + 1 running ⇒ only the 2 exited are listed (the prune-candidate set).
    let h = TmpHome::new("prune-mix");
    let a = make_run_dir(h.path(), "exited-a", None);
    mark_exited(&a, 0);
    let b = make_run_dir(h.path(), "exited-b", None);
    mark_exited(&b, 1);
    let r = make_run_dir(h.path(), "running-c", None);
    mark_running(&r);

    let mut got = list_stopped_runs(h.path()).unwrap();
    got.sort();
    assert_eq!(got, vec!["exited-a".to_string(), "exited-b".to_string()]);
}

#[test]
fn list_stopped_runs_skips_names_dir_and_unknown() {
    // The `names` registry sub-dir is not a run; a fresh dir (no status) is
    // indeterminate and excluded (fail-closed: only proven-exited runs listed).
    let h = TmpHome::new("prune-skip");
    let e = make_run_dir(h.path(), "exited-x", Some("web"));
    mark_exited(&e, 0);
    super::super::registry::claim(h.path(), "web", "exited-x").unwrap();
    make_run_dir(h.path(), "fresh-y", None); // no status ⇒ Unknown, excluded

    let got = list_stopped_runs(h.path()).unwrap();
    assert_eq!(got, vec!["exited-x".to_string()]);
}

#[test]
fn wait_then_exit_observed_via_background_writer() {
    // A run that is "running", then a concurrent writer marks it exited — wait_run
    // must observe the terminal status and return the code. Uses the same
    // fake-running trick (our own pid alive) and a thread that flips the status.
    use std::time::Duration;
    let h = TmpHome::new("wait-flip");
    let dir = make_run_dir(h.path(), "id-flip", None);
    mark_running(&dir);

    let dir2 = dir.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        // Remove the ctl endpoint (no longer running) AND write the exit status,
        // mirroring the supervisor's teardown order.
        let _ = std::fs::remove_file(super::ctl_sock_path(&dir2));
        std::fs::write(dir2.join("status"), "exited 5").unwrap();
    });

    let code = wait_run(h.path(), "id-flip").unwrap();
    writer.join().unwrap();
    assert_eq!(code, 5);
}
