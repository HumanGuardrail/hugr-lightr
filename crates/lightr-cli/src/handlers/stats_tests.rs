//! Tests for `lightr stats`. The env-dependent paths (the handler reads
//! `LIGHTR_HOME` via `lightr_home()`) run under the shared `ENV_LOCK` — the
//! house convention (see inspect_tests / inspect.rs) — so they never race other
//! env-touching tests. A real short-lived `sleep` child backs the "running"
//! case so the `ps` shell-out is exercised end-to-end.

use super::run as stats_run;
use crate::test_lock::ENV_LOCK;
use std::fs;
use std::process::{Child, Command};

/// Create a run dir resolvable by `lightr_run::resolve` (needs the dir +
/// spec.json so `ps()` lists it) and write a `pid` file pointing at `pid`.
fn make_run(home: &std::path::Path, id: &str, pid: Option<u32>, running: bool) {
    let run_dir = home.join("run").join(id);
    fs::create_dir_all(&run_dir).unwrap();
    let spec = serde_json::json!({
        "cwd": "/work",
        "command": ["sleep", "30"],
        "env_keys": [],
        "mounts": [],
        "detached": true,
        "created_at_unix": 1_717_600_000u64,
        "ports": [],
        "engine": "native",
        "rootfs_ref": null,
        "env": []
    });
    fs::write(run_dir.join("spec.json"), spec.to_string()).unwrap();
    if let Some(p) = pid {
        fs::write(run_dir.join("pid"), p.to_string()).unwrap();
    }
    // `ps()` treats a run as running iff the ctl socket exists AND the pid is
    // alive. We don't create a real socket here; the no-target test asserts the
    // honest empty/running shape via exit code, and the targeted test drives the
    // pid path directly (which does not depend on the socket).
    if running {
        // touch a sentinel so the layout matches a live run dir.
        fs::write(run_dir.join("status"), "running").unwrap();
    }
}

fn spawn_sleeper() -> Child {
    Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep child")
}

// ── running target → real ps metrics, exit 0 ────────────────────────────────

#[test]
fn stats_running_target_reports_metrics() {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = spawn_sleeper();
    let id = "1717600000000000010-1";
    make_run(tmp.path(), id, Some(child.id()), true);

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: single-threaded under ENV_LOCK.
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = stats_run(Some(id));
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(code, 0, "stats on a running target must exit 0");
}

// ── stopped target (pid file gone / dead) → honest row, exit 0 ──────────────

#[test]
fn stats_stopped_target_is_honest_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let id = "1717600000000000011-2";
    make_run(tmp.path(), id, None, false); // no pid file → resting row

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = stats_run(Some(id));
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 0, "stats on a stopped (known) target still exits 0");
}

// ── absent target → "No such container", exit 1 ─────────────────────────────

#[test]
fn stats_absent_target_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("run")).unwrap();

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = stats_run(Some("no-such-id"));
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 1, "stats on an unknown target must exit 1");
}

// ── no target → lists running, exit 0 (even when empty) ─────────────────────

#[test]
fn stats_no_target_lists_and_exits_0() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("run")).unwrap();

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = stats_run(None);
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 0, "stats with no target lists running runs, exits 0");
}
