//! Tests for memo key properties: stability, port exclusion, input/arg/env
//! sensitivity, and predict correctness.
#![cfg(test)]

use crate::run::memo::{build_key, predict, run_memoized};
use crate::run::types::{PortMap, RunSpec};
use lightr_store::Store;
use std::fs;

// LIGHTR_HOME is process-global (index dir): serialized via super::ENV_LOCK
// (shared across all sibling test modules in the same binary).

fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = super::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("LIGHTR_HOME", home.path());
    (home, guard)
}

fn make_store(dir: &std::path::Path) -> Store {
    Store::open(dir.join("store")).expect("store open")
}

fn make_spec(cwd: &std::path::Path, command: Vec<&str>) -> RunSpec {
    RunSpec {
        cwd: cwd.to_path_buf(),
        inputs: vec![],
        command: command.into_iter().map(|s| s.to_string()).collect(),
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
    }
}

// -----------------------------------------------------------------------
// key_stability: same spec twice => same key via two scans
// -----------------------------------------------------------------------
#[test]
fn key_stability() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    // Create a file so the scan has something to digest
    fs::write(cwd.join("file.txt"), b"hello").unwrap();

    let spec = make_spec(cwd, vec!["/bin/echo", "hello"]);
    let k1 = build_key(&spec).expect("key1");
    let k2 = build_key(&spec).expect("key2");
    assert_eq!(k1.0, k2.0, "same spec must produce same key");
}

// -----------------------------------------------------------------------
// MEMO-KEY LAW: ports are RUNTIME, not a key input (like resource limits;
// like Docker, which does not key on -p). Two specs differing ONLY in
// `ports` MUST produce the same memo key.
// -----------------------------------------------------------------------
#[test]
fn ports_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let mut spec_no_ports = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_no_ports.ports = vec![];

    let mut spec_with_ports = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_with_ports.ports = vec![
        PortMap {
            host: 8080,
            container: 80,
        },
        PortMap {
            host: 9090,
            container: 90,
        },
    ];

    let k1 = build_key(&spec_no_ports).expect("k1");
    let k2 = build_key(&spec_with_ports).expect("k2");
    assert_eq!(
        k1.0, k2.0,
        "ports must NOT affect the memo key (runtime-only, like -p in Docker)"
    );
}

// -----------------------------------------------------------------------
// key_changes_when_input_file_changes
// -----------------------------------------------------------------------
#[test]
fn key_changes_when_input_file_changes() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("data.txt"), b"version1").unwrap();

    let spec = make_spec(cwd, vec!["/bin/echo", "x"]);
    let k1 = build_key(&spec).expect("k1");

    fs::write(cwd.join("data.txt"), b"version2").unwrap();
    let k2 = build_key(&spec).expect("k2");

    assert_ne!(
        k1.0, k2.0,
        "key must change when input file content changes"
    );
}

// -----------------------------------------------------------------------
// key_changes_when_arg_changes
// -----------------------------------------------------------------------
#[test]
fn key_changes_when_arg_changes() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let spec1 = make_spec(cwd, vec!["/bin/echo", "argA"]);
    let spec2 = make_spec(cwd, vec!["/bin/echo", "argB"]);

    let k1 = build_key(&spec1).expect("k1");
    let k2 = build_key(&spec2).expect("k2");
    assert_ne!(k1.0, k2.0, "key must change when args change");
}

// -----------------------------------------------------------------------
// key_changes_when_selected_env_changes
// -----------------------------------------------------------------------
#[test]
fn key_changes_when_selected_env_changes() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    std::env::set_var("LIGHTR_TEST_VAR_KCW", "valueA");
    let spec1 = RunSpec {
        cwd: cwd.to_path_buf(),
        inputs: vec![],
        command: vec!["/bin/echo".to_string(), "x".to_string()],
        env_keys: vec!["LIGHTR_TEST_VAR_KCW".to_string()],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
    };
    let k1 = build_key(&spec1).expect("k1");

    std::env::set_var("LIGHTR_TEST_VAR_KCW", "valueB");
    let k2 = build_key(&spec1).expect("k2");

    std::env::remove_var("LIGHTR_TEST_VAR_KCW");
    assert_ne!(
        k1.0, k2.0,
        "key must change when selected env value changes"
    );
}

// -----------------------------------------------------------------------
// predict: miss → run → predict hit
// -----------------------------------------------------------------------
#[test]
fn predict_miss_run_hit() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();

    let spec = RunSpec {
        cwd: work.clone(),
        inputs: vec![],
        command: vec!["/bin/echo".to_string(), "predict-test".to_string()],
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
    };

    let (key1, hit1) = predict(&spec, &store).expect("predict1");
    assert!(!hit1, "predict before run must be miss");

    let out = run_memoized(&spec, &store).expect("run");
    assert!(!out.hit, "first run must be miss");
    assert_eq!(out.key, key1, "predict key must match run key");

    let (key2, hit2) = predict(&spec, &store).expect("predict2");
    assert_eq!(key1, key2, "key must be stable");
    assert!(hit2, "predict after run must be hit");
}
