//! Tests for `lightr top`. Env-dependent paths run under `ENV_LOCK` (house
//! convention). A real `sleep` child backs the "running" case so the `ps` /
//! `pgrep` shell-outs are exercised end-to-end.

use super::run as top_run;
use crate::test_lock::ENV_LOCK;
use std::fs;
use std::process::{Child, Command};

fn make_run(home: &std::path::Path, id: &str, pid: Option<u32>) {
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
}

fn spawn_sleeper() -> Child {
    Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep child")
}

// ── running target → process table, exit 0 ──────────────────────────────────

#[test]
fn top_running_target_lists_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = spawn_sleeper();
    let id = "1717600000000000020-1";
    make_run(tmp.path(), id, Some(child.id()));

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = top_run(id);
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(code, 0, "top on a running target must exit 0");
}

// ── not-running (no pid file) → error, exit 1 ───────────────────────────────

#[test]
fn top_not_running_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    let id = "1717600000000000021-2";
    make_run(tmp.path(), id, None); // no pid file → not running

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = top_run(id);
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 1, "top on a not-running container must exit 1");
}

// ── dead pid (process already gone) → error, exit 1 ─────────────────────────

#[test]
fn top_dead_pid_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    // Spawn + reap a child so its pid is (almost certainly) gone.
    let mut child = spawn_sleeper();
    let dead_pid = child.id();
    let _ = child.kill();
    let _ = child.wait();

    let id = "1717600000000000022-3";
    make_run(tmp.path(), id, Some(dead_pid));

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = top_run(id);
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(
        code, 1,
        "top on a dead pid must exit 1 (honestly not running)"
    );
}

// ── absent target → error, exit 1 ───────────────────────────────────────────

#[test]
fn top_absent_target_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("run")).unwrap();

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = top_run("no-such-id");
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 1, "top on an unknown target must exit 1");
}
