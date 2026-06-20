//! Tests for deep-memo: availability probe, disabled/enabled fallback.
#![cfg(test)]

use crate::run::deepmemo::{deep_memo_available, run_memoized_deep};
use crate::run::memo::run_memoized;
use crate::run::types::{DeepMemoConfig, RunSpec};
use lightr_store::Store;
use std::fs;

fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = super::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("LIGHTR_HOME", home.path());
    (home, guard)
}

fn make_store(dir: &std::path::Path) -> Store {
    Store::open(dir.join("store")).expect("store open")
}

// deep_memo_disabled_equals_run_memoized:
// run_memoized_deep(cfg.enabled=false) must produce same key and hit
// behaviour as run_memoized — miss on first call, hit on second.
#[test]
fn deep_memo_disabled_equals_run_memoized() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();

    let side_effect = tmp.path().join("dm_disabled_side.txt");
    let spec = RunSpec {
        cwd: work.clone(),
        inputs: vec![],
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo deep >> {}", side_effect.display()),
        ],
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
        env_explicit: vec![],
        workdir: None,
        user: None,
        restart: None,
        stop_signal: None,
        ..Default::default()
    };
    let cfg = DeepMemoConfig { enabled: false };

    // First call: miss (same as run_memoized miss)
    let out1 = run_memoized_deep(&spec, &store, &cfg).expect("deep miss");
    assert!(!out1.hit, "disabled deep-memo first call must be miss");
    assert_eq!(out1.exit_code, 0);

    // Second call: hit (run_memoized would also hit)
    let out2 = run_memoized_deep(&spec, &store, &cfg).expect("deep hit");
    assert!(out2.hit, "disabled deep-memo second call must be hit");
    assert_eq!(out2.key, out1.key, "key must be stable across calls");

    // Verify same key as plain run_memoized would produce
    let out_plain = run_memoized(&spec, &store).expect("plain hit");
    assert!(out_plain.hit, "plain run_memoized should also hit");
    assert_eq!(
        out1.key, out_plain.key,
        "deep disabled key must match plain key"
    );

    // Side-effect written once (command did not re-execute on hit)
    let line_count = fs::read_to_string(&side_effect)
        .unwrap_or_default()
        .lines()
        .count();
    assert_eq!(line_count, 1, "side effect must be written exactly once");
}

// deep_memo_available_returns_false_with_shim_reason:
// On this host (no shim installed), deep_memo_available() must return
// (false, reason) where reason is non-empty and mentions "shim" or "unavailable".
#[test]
fn deep_memo_available_returns_false_with_shim_reason() {
    let (_home, _env_guard) = isolated_home();
    let (available, reason) = deep_memo_available();
    assert!(
        !available,
        "deep_memo_available must return false on R4 host"
    );
    assert!(!reason.is_empty(), "reason must be non-empty");
    let reason_lower = reason.to_lowercase();
    assert!(
        reason_lower.contains("shim") || reason_lower.contains("unavailable"),
        "reason must mention 'shim' or 'unavailable', got: {reason:?}"
    );
}

// deep_memo_enabled_fallback_correctness:
// run_memoized_deep(cfg.enabled=true) on this host falls back to
// whole-run memo: miss then hit; deep_memo_available() confirms (false, reason).
#[test]
fn deep_memo_enabled_fallback_correctness() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();

    let side_effect = tmp.path().join("dm_enabled_side.txt");
    let spec = RunSpec {
        cwd: work.clone(),
        inputs: vec![],
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo enabled >> {}", side_effect.display()),
        ],
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
        env_explicit: vec![],
        workdir: None,
        user: None,
        restart: None,
        stop_signal: None,
        ..Default::default()
    };
    let cfg_on = DeepMemoConfig { enabled: true };

    // Confirm probe says unavailable before we call the function
    let (available, reason) = deep_memo_available();
    assert!(!available);
    assert!(!reason.is_empty());

    // First call with enabled=true: should return Ok, fall back to miss
    let out1 = run_memoized_deep(&spec, &store, &cfg_on).expect("enabled call 1 must not err");
    assert!(
        !out1.hit,
        "first enabled call must be miss (fallback to whole-run memo)"
    );
    assert_eq!(out1.exit_code, 0);

    // Second call with enabled=true: should hit (whole-run memo populated)
    let out2 = run_memoized_deep(&spec, &store, &cfg_on).expect("enabled call 2 must not err");
    assert!(
        out2.hit,
        "second enabled call must be hit (fallback memoized)"
    );
    assert_eq!(out2.key, out1.key, "keys must be stable");

    // Side-effect written once (no double-exec)
    let line_count = fs::read_to_string(&side_effect)
        .unwrap_or_default()
        .lines()
        .count();
    assert_eq!(line_count, 1, "side effect must be written exactly once");
}
