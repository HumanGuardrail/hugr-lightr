//! Tests for memo run-execution semantics: miss/hit, exit-nonzero,
//! output-cap, corrupt AC, mount escape + mount run/key change.
#![cfg(test)]

use crate::run::memo::{build_key, run_memoized, validate_mount_target};
use crate::run::types::{Mount, RunSpec};
use lightr_core::OUTPUT_CAP_BYTES;
use lightr_store::Store;
use std::fs;
use std::io::Write;

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

// -----------------------------------------------------------------------
// miss_then_hit: run twice; side-effect file written once; 2nd run is HIT
// -----------------------------------------------------------------------
#[test]
fn miss_then_hit() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();
    let cwd = work.as_path();
    let store = make_store(tmp.path());

    let side_effect = tmp.path().join("side_effect.txt");

    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("echo hit >> {}", side_effect.display()),
    ];

    let spec = RunSpec {
        cwd: cwd.to_path_buf(),
        inputs: vec![],
        command: cmd,
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
    };

    let out1 = run_memoized(&spec, &store).expect("run1");
    assert!(!out1.hit, "first run must be miss");
    assert_eq!(out1.exit_code, 0);

    let contents1 = fs::read_to_string(&side_effect).unwrap_or_default();
    assert_eq!(
        contents1.lines().count(),
        1,
        "side effect written once after first run"
    );

    let out2 = run_memoized(&spec, &store).expect("run2");
    assert!(out2.hit, "second run must be hit");
    assert_eq!(out2.exit_code, 0);
    assert_eq!(out1.stdout, out2.stdout, "replayed stdout must match");

    let contents2 = fs::read_to_string(&side_effect).unwrap_or_default();
    assert_eq!(
        contents2.lines().count(),
        1,
        "side effect must not be re-written on hit"
    );
}

// -----------------------------------------------------------------------
// exit_nonzero_never_memoized: exit-7 cmd twice, both MISS, side-effect written twice
// -----------------------------------------------------------------------
#[test]
fn exit_nonzero_never_memoized() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();
    let cwd = work.as_path();
    let store = make_store(tmp.path());

    let side_effect = tmp.path().join("side_effect_fail.txt");
    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("echo fail >> {}; exit 7", side_effect.display()),
    ];
    let spec = RunSpec {
        cwd: cwd.to_path_buf(),
        inputs: vec![],
        command: cmd,
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
    };

    let out1 = run_memoized(&spec, &store).expect("run1");
    assert!(!out1.hit, "first run must be miss");
    assert_eq!(out1.exit_code, 7);

    let out2 = run_memoized(&spec, &store).expect("run2");
    assert!(!out2.hit, "second run must also be miss (not memoized)");
    assert_eq!(out2.exit_code, 7);

    let contents = fs::read_to_string(&side_effect).unwrap_or_default();
    assert_eq!(
        contents.lines().count(),
        2,
        "side effect must be written twice"
    );
}

// -----------------------------------------------------------------------
// output_cap_not_memoized: >5MiB stdout not memoized
// -----------------------------------------------------------------------
#[test]
fn output_cap_not_memoized() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();
    let cwd = work.as_path();
    let store = make_store(tmp.path());

    let side_effect = tmp.path().join("side_effect_cap.txt");
    let large_file = tmp.path().join("large.bin");
    {
        let mut f = fs::File::create(&large_file).unwrap();
        f.write_all(&vec![b'x'; OUTPUT_CAP_BYTES + 1]).unwrap();
    }

    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "cat {} && echo side >> {}",
            large_file.display(),
            side_effect.display()
        ),
    ];
    let spec = RunSpec {
        cwd: cwd.to_path_buf(),
        inputs: vec![],
        command: cmd,
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
    };

    let out1 = run_memoized(&spec, &store).expect("run1");
    assert!(!out1.hit, "first run must be miss");
    assert_eq!(out1.exit_code, 0);
    assert!(
        out1.stdout.len() > OUTPUT_CAP_BYTES,
        "stdout must exceed cap"
    );

    let out2 = run_memoized(&spec, &store).expect("run2");
    assert!(
        !out2.hit,
        "second run must also be miss (output cap exceeded)"
    );

    let contents = fs::read_to_string(&side_effect).unwrap_or_default();
    assert_eq!(
        contents.lines().count(),
        2,
        "side effect must be written twice when output cap exceeded"
    );
}

// -----------------------------------------------------------------------
// corrupt_ac_record_treated_as_miss: flip 1 byte in AC record => miss not error
// -----------------------------------------------------------------------
#[test]
fn corrupt_ac_record_treated_as_miss() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();
    let cwd = work.as_path();
    let store = make_store(tmp.path());

    let side_effect = tmp.path().join("side_effect_corrupt.txt");
    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("echo ok >> {}", side_effect.display()),
    ];
    let spec = RunSpec {
        cwd: cwd.to_path_buf(),
        inputs: vec![],
        command: cmd,
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
    };

    let out1 = run_memoized(&spec, &store).expect("run1");
    assert!(!out1.hit);
    assert_eq!(out1.exit_code, 0);

    let key = build_key(&spec).expect("key");
    let record = store.ac_get(&key).expect("ac_get").expect("record present");
    let mut corrupt = record.clone();
    corrupt[0] ^= 0xFF;
    store.ac_put(&key, &corrupt).expect("ac_put");

    let out3 = run_memoized(&spec, &store).expect("run3 must not error");
    assert!(!out3.hit, "corrupt AC record must be treated as miss");
    assert_eq!(out3.exit_code, 0);

    let contents = fs::read_to_string(&side_effect).unwrap_or_default();
    assert_eq!(
        contents.lines().count(),
        2,
        "command executed on miss and after corrupt"
    );
}

// -----------------------------------------------------------------------
// mount_escape_rejected: mount target with ".." rejected
// -----------------------------------------------------------------------
#[test]
fn mount_escape_rejected() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();

    assert!(
        validate_mount_target("../escape").is_err(),
        "mount target with '..' must be rejected"
    );
    assert!(
        validate_mount_target("a/../../escape").is_err(),
        "escaping via a/../../ must be rejected"
    );
    assert!(validate_mount_target("subdir").is_ok());
    assert!(validate_mount_target("a/b/c").is_ok());
    assert!(
        validate_mount_target("/abs").is_err(),
        "absolute path must be rejected"
    );

    let _ = cwd;
}

// -----------------------------------------------------------------------
// mounts_run: run with mount of snapshotted ref → file present in cwd/target
//             + key changes when mount ref repointed
// -----------------------------------------------------------------------
#[test]
fn mounts_run_and_key_change() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    let tmp = tempfile::tempdir().unwrap();

    let src_v1 = tmp.path().join("src_v1");
    fs::create_dir(&src_v1).unwrap();
    fs::write(src_v1.join("hello.txt"), b"hello from v1").unwrap();
    lightr_index::snapshot(&src_v1, &store, "testmount").expect("snapshot v1");

    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();

    let spec = RunSpec {
        cwd: work.clone(),
        inputs: vec![],
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "cat mounted/hello.txt".to_string(),
        ],
        env_keys: vec![],
        mounts: vec![Mount {
            ref_name: "testmount".to_string(),
            target: "mounted".to_string(),
        }],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
    };

    let out1 = run_memoized(&spec, &store).expect("run1 with mount");
    assert_eq!(out1.exit_code, 0, "mounted run should exit 0");
    assert!(
        out1.stdout.starts_with(b"hello from v1"),
        "stdout should contain file content"
    );

    let key1 = out1.key;

    let src_v2 = tmp.path().join("src_v2");
    fs::create_dir(&src_v2).unwrap();
    fs::write(src_v2.join("hello.txt"), b"hello from v2").unwrap();
    lightr_index::snapshot(&src_v2, &store, "testmount").expect("snapshot v2");

    let mounted_dir = work.join("mounted");
    if mounted_dir.exists() {
        fs::remove_dir_all(&mounted_dir).unwrap();
    }

    let out2 = run_memoized(&spec, &store).expect("run2 with mount v2");
    assert_eq!(out2.exit_code, 0);
    assert!(
        out2.stdout.starts_with(b"hello from v2"),
        "stdout should contain v2 content"
    );
    assert_ne!(
        key1, out2.key,
        "key must change when mount ref is repointed"
    );
}
