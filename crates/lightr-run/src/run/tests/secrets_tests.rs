//! Tests for F-309 secrets/configs: key contribution, hydration, fail-closed.
#![cfg(test)]

use crate::run::memo::{predict, run_memoized_with};
use crate::run::types::{RunSpec, StoreFile};
use crate::secrets;
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

/// Snapshot a dir holding one file `<file_name>` with `bytes` as ref `name`.
/// Returns the ref's root digest.
fn snapshot_file_ref(
    store: &Store,
    name: &str,
    file_name: &str,
    bytes: &[u8],
) -> lightr_core::Digest {
    let src = tempfile::tempdir().unwrap();
    fs::write(src.path().join(file_name), bytes).unwrap();
    let rep = lightr_index::snapshot(src.path(), store, name).expect("snapshot ref");
    rep.root
}

// Changing a secret REF must change the memo key (cache miss). Two specs
// differing only in a secret ref must produce different keys (§0).
#[test]
fn secret_ref_changes_memo_key() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    // Two distinct refs (different content ⇒ different root digest).
    snapshot_file_ref(&store, "sec-a", "token", b"AAAA");
    snapshot_file_ref(&store, "sec-b", "token", b"BBBB");

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();

    let mk = |ref_name: &str| RunSpec {
        cwd: work.clone(),
        inputs: vec![],
        command: vec!["/bin/echo".to_string(), "x".to_string()],
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![StoreFile {
            name: "token".to_string(),
            ref_name: ref_name.to_string(),
        }],
        configs: vec![],
        ports: vec![],
        env_explicit: vec![],
        workdir: None,
        user: None,
        restart: None,
        stop_signal: None,
        ..Default::default()
    };

    let spec_a = mk("sec-a");
    let spec_b = mk("sec-b");

    // predict computes the key without executing (routes through assemble_key
    // because secrets is non-empty).
    let (key_a, _) = predict(&spec_a, &store).expect("predict a");
    let (key_b, _) = predict(&spec_b, &store).expect("predict b");
    assert_ne!(
        key_a, key_b,
        "a different secret ref must produce a different memo key"
    );

    // And the same secret ref is stable.
    let (key_a2, _) = predict(&spec_a, &store).expect("predict a2");
    assert_eq!(key_a, key_a2, "same secret ref ⇒ stable key");
}

// A config ref likewise contributes to the key, in its own domain (so a
// secret and a config with the SAME name+ref do not collide).
#[test]
fn config_ref_changes_memo_key_and_domain_separated() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    snapshot_file_ref(&store, "cfg-ref", "data", b"hello");

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();

    let base = || RunSpec {
        cwd: work.clone(),
        inputs: vec![],
        command: vec!["/bin/echo".to_string(), "x".to_string()],
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

    let mut as_secret = base();
    as_secret.secrets = vec![StoreFile {
        name: "f".to_string(),
        ref_name: "cfg-ref".to_string(),
    }];
    let mut as_config = base();
    as_config.configs = vec![StoreFile {
        name: "f".to_string(),
        ref_name: "cfg-ref".to_string(),
    }];

    let (key_secret, _) = predict(&as_secret, &store).expect("predict secret");
    let (key_config, _) = predict(&as_config, &store).expect("predict config");
    let (key_none, _) = predict(&base(), &store).expect("predict none");

    assert_ne!(key_secret, key_none, "a secret must change the key");
    assert_ne!(key_config, key_none, "a config must change the key");
    assert_ne!(
        key_secret, key_config,
        "secret vs config domains must be separated (same name+ref must not collide)"
    );
}

// Empty secrets/configs ⇒ key is byte-identical to a spec with no F-309
// fields, i.e. the storeless fast path. Guards the 16 existing callers.
#[test]
fn empty_secrets_configs_leave_key_unchanged() {
    use crate::run::memo::build_key;

    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();
    fs::write(work.join("f.txt"), b"data").unwrap();

    let spec = RunSpec {
        cwd: work.clone(),
        inputs: vec![],
        command: vec!["/bin/echo".to_string(), "x".to_string()],
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
    // build_key is the storeless fast path (no mounts/secrets/configs).
    let fast = build_key(&spec).expect("fast key");
    // predict routes through the same fast path when there are no
    // store-backed inputs; it must agree byte-for-byte.
    let (predicted, _) = predict(&spec, &store).expect("predict");
    assert_eq!(
        fast, predicted,
        "empty secrets/configs ⇒ fast path key == predict key"
    );
}

// Secret hydrated to <cwd>/.lightr/secrets/<name> at 0600; config at
// <cwd>/.lightr/configs/<name> at 0644 (unix). The ref is a snapshot tree,
// so <name> is a dir holding the snapshot's file at the requested mode.
#[test]
fn secret_config_hydrate_path_and_mode() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    snapshot_file_ref(&store, "my-secret", "token.txt", b"s3cr3t");
    snapshot_file_ref(&store, "my-config", "app.conf", b"k=v");

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();

    secrets::hydrate(
        &work,
        &store,
        &[StoreFile {
            name: "sec".to_string(),
            ref_name: "my-secret".to_string(),
        }],
        &[StoreFile {
            name: "cfg".to_string(),
            ref_name: "my-config".to_string(),
        }],
    )
    .expect("hydrate ok");

    let secret_file = work.join(".lightr/secrets/sec/token.txt");
    let config_file = work.join(".lightr/configs/cfg/app.conf");
    assert!(secret_file.exists(), "secret file must be materialized");
    assert!(config_file.exists(), "config file must be materialized");
    assert_eq!(fs::read(&secret_file).unwrap(), b"s3cr3t");
    assert_eq!(fs::read(&config_file).unwrap(), b"k=v");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let smode = fs::metadata(&secret_file).unwrap().permissions().mode() & 0o777;
        let cmode = fs::metadata(&config_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(smode, 0o600, "secret file must be 0600, got {smode:o}");
        assert_eq!(cmode, 0o644, "config file must be 0644, got {cmode:o}");
    }
}

// A missing secret ref must fail CLOSED (Err), no run proceeds.
#[test]
fn missing_secret_ref_fails_closed() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();
    let store = make_store(&home_path);

    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();

    let err = secrets::hydrate(
        &work,
        &store,
        &[StoreFile {
            name: "sec".to_string(),
            ref_name: "no-such-ref".to_string(),
        }],
        &[],
    );
    assert!(err.is_err(), "missing secret ref must fail closed");

    // And via run_memoized_with: a missing secret aborts the whole run.
    let spec = RunSpec {
        cwd: work.clone(),
        inputs: vec![],
        command: vec!["/bin/echo".to_string(), "x".to_string()],
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![StoreFile {
            name: "sec".to_string(),
            ref_name: "no-such-ref".to_string(),
        }],
        configs: vec![],
        ports: vec![],
        env_explicit: vec![],
        workdir: None,
        user: None,
        restart: None,
        stop_signal: None,
        ..Default::default()
    };
    let run_err = run_memoized_with(&spec, &store, &lightr_core::ResourceLimits::default());
    assert!(run_err.is_err(), "run with a missing secret must Err");
}
