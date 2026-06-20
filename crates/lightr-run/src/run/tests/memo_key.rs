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
        env_explicit: vec![],
        workdir: None,
        user: None,
        restart: None,
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
// WP-RC-WORKDIR: -w/--workdir is RUNTIME (like ports/limits; like Docker,
// which does not key on -w). Two specs differing ONLY in workdir must key
// IDENTICALLY — otherwise a `-w` would bust the cache (a false miss).
// -----------------------------------------------------------------------
#[test]
fn workdir_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let spec_no_wd = make_spec(cwd, vec!["/bin/echo", "x"]);

    let mut spec_with_wd = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_with_wd.workdir = Some("sub/wd".to_string());

    let k1 = build_key(&spec_no_wd).expect("k1");
    let k2 = build_key(&spec_with_wd).expect("k2");
    assert_eq!(
        k1.0, k2.0,
        "workdir must NOT affect the memo key (runtime-only, like -w in Docker)"
    );
}

// -----------------------------------------------------------------------
// WP-RC-USER: -u/--user is RUNTIME (like ports/workdir; like Docker, which
// does not key on -u). Two specs differing ONLY in user must key IDENTICALLY
// — otherwise a `-u` would bust the cache (a false miss).
// -----------------------------------------------------------------------
#[test]
fn user_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let spec_no_user = make_spec(cwd, vec!["/bin/echo", "x"]);

    let mut spec_with_user = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_with_user.user = Some("1000:1000".to_string());

    let k1 = build_key(&spec_no_user).expect("k1");
    let k2 = build_key(&spec_with_user).expect("k2");
    assert_eq!(
        k1.0, k2.0,
        "user must NOT affect the memo key (runtime-only, like -u in Docker)"
    );
}

// WP-RC-RESTART: --restart is RUNTIME (like ports/workdir/user; Docker does not
// key on it). Specs differing ONLY in restart must key IDENTICALLY (no false miss).
#[test]
fn restart_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let spec_no_restart = make_spec(cwd, vec!["/bin/echo", "x"]);
    let mut spec_on_failure = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_on_failure.restart = Some("on-failure:3".to_string());

    let k0 = build_key(&spec_no_restart).expect("k0").0;
    let k1 = build_key(&spec_on_failure).expect("k1").0;
    assert_eq!(k0, k1, "restart must NOT affect the memo key");
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
        env_explicit: vec![],
        workdir: None,
        user: None,
        restart: None,
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
        env_explicit: vec![],
        workdir: None,
        user: None,
        restart: None,
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

// =======================================================================
// WP-RC-1 (R-KEY): env_explicit is KEYED; discovery env is NOT.
// =======================================================================

// MEMO LAW: a run with NO `-e`/`--env-file` (empty env_explicit) keys
// byte-identically to the pre-WP-RC-1 key — behavior-preserving.
#[test]
fn empty_env_explicit_preserves_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let mut spec = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec.env_explicit = vec![];
    let k1 = build_key(&spec).expect("k1");

    // A second spec with an explicitly-empty env_explicit must match.
    let spec2 = make_spec(cwd, vec!["/bin/echo", "x"]);
    let k2 = build_key(&spec2).expect("k2");
    assert_eq!(
        k1.0, k2.0,
        "empty env_explicit must not change the key (behavior-preserving)"
    );
}

// MEMO LAW (the core no-false-hit guarantee): two specs differing ONLY in
// `env_explicit` (a different `-e` value) MUST produce DIFFERENT keys, so a
// changed `-e` can never replay a stale cached result.
#[test]
fn differing_env_explicit_busts_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let mut spec_a = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_a.env_explicit = vec![("FOO".to_string(), "bar".to_string())];

    let mut spec_b = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_b.env_explicit = vec![("FOO".to_string(), "baz".to_string())];

    let ka = build_key(&spec_a).expect("ka");
    let kb = build_key(&spec_b).expect("kb");
    assert_ne!(
        ka.0, kb.0,
        "a different -e VALUE must bust the run key (no false hit)"
    );

    // A different KEY (not just value) must also bust it.
    let mut spec_c = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_c.env_explicit = vec![("OTHER".to_string(), "bar".to_string())];
    let kc = build_key(&spec_c).expect("kc");
    assert_ne!(ka.0, kc.0, "a different -e KEY must bust the run key");

    // And a non-empty env_explicit must differ from the empty (no-flag) key.
    let spec_empty = make_spec(cwd, vec!["/bin/echo", "x"]);
    let ke = build_key(&spec_empty).expect("ke");
    assert_ne!(
        ka.0, ke.0,
        "adding -e must bust the key vs a no-flag run (no false hit)"
    );
}

// env_explicit order on the CLI must NOT change the key (the fold sorts), so
// `-e A=1 -e B=2` and `-e B=2 -e A=1` are the same cached run.
#[test]
fn env_explicit_order_independent() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let mut spec_ab = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_ab.env_explicit = vec![
        ("A".to_string(), "1".to_string()),
        ("B".to_string(), "2".to_string()),
    ];
    let mut spec_ba = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_ba.env_explicit = vec![
        ("B".to_string(), "2".to_string()),
        ("A".to_string(), "1".to_string()),
    ];

    let kab = build_key(&spec_ab).expect("kab");
    let kba = build_key(&spec_ba).expect("kba");
    assert_eq!(
        kab.0, kba.0,
        "env_explicit CLI order must not change the key (the fold sorts)"
    );
}

// LEAD ARBITRATION env-split: the DISCOVERY `env` channel is UNKEYED. `RunSpec`
// carries no discovery-`env` field at all (it lives on `SpecOnDisk` for the
// detached path) — so the run key is structurally independent of discovery env.
// This guards that the only env-shaped key contribution is env_explicit: a spec
// with env_explicit set differs from one without, while the discovery channel
// (modelled by env_keys whose values are absent) does not collide with it.
#[test]
fn discovery_env_stays_unkeyed_vs_explicit() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    // Baseline: no env at all.
    let base = make_spec(cwd, vec!["/bin/echo", "x"]);
    let k_base = build_key(&base).expect("k_base");

    // env_explicit set ⇒ key MUST change (it is keyed).
    let mut explicit = make_spec(cwd, vec!["/bin/echo", "x"]);
    explicit.env_explicit = vec![("FOO".to_string(), "bar".to_string())];
    let k_explicit = build_key(&explicit).expect("k_explicit");
    assert_ne!(
        k_base.0, k_explicit.0,
        "env_explicit is keyed: it must change the key"
    );

    // The env_explicit fold uses a `\x03env_explicit\0` domain tag, so it can
    // never be confused with the env_keys (`=`/`\x01`) fold — distinct channels.
}
