use super::*;
use std::fs::{self, Permissions};
use lightr_core::{Digest, LightrError};
use tempfile::TempDir;
use crate::Store;

fn tmp_store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

// ── object plane ────────────────────────────────────────────────────────

#[test]
fn roundtrip_put_get() {
    let (_dir, store) = tmp_store();
    let data = b"hello, lightr!";
    let d = store.put_bytes(data).unwrap();
    let got = store.get_bytes(&d).unwrap();
    assert_eq!(&got[..], data);
}

#[test]
fn idempotent_double_put() {
    let (_dir, store) = tmp_store();
    let data = b"idempotent data";
    let d1 = store.put_bytes(data).unwrap();
    let d2 = store.put_bytes(data).unwrap();
    assert_eq!(d1, d2);
    // Verify get still works.
    assert_eq!(store.get_bytes(&d1).unwrap(), data);
}

#[test]
fn integrity_corruption() {
    let (_dir, store) = tmp_store();
    let data = b"tamper me";
    let d = store.put_bytes(data).unwrap();

    // Locate the object file, relax permissions, flip a byte, restore.
    let obj_path = object_path(&store.root, &d);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&obj_path, Permissions::from_mode(0o644)).unwrap();
    }
    #[cfg(windows)]
    {
        let mut perms = fs::metadata(&obj_path).unwrap().permissions();
        perms.set_readonly(false);
        fs::set_permissions(&obj_path, perms).unwrap();
    }
    let mut bytes = fs::read(&obj_path).unwrap();
    bytes[0] ^= 0xFF;
    fs::write(&obj_path, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&obj_path, Permissions::from_mode(0o444)).unwrap();
    }
    #[cfg(windows)]
    {
        let mut perms = fs::metadata(&obj_path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&obj_path, perms).unwrap();
    }

    let err = store.get_bytes(&d).unwrap_err();
    match err {
        LightrError::Integrity { expected, actual } => {
            assert_eq!(expected, d);
            assert_ne!(actual, d);
        }
        other => panic!("expected Integrity, got {:?}", other),
    }

    // Evidence file must still be present (never deleted).
    assert!(
        obj_path.exists(),
        "evidence file was deleted — violates spec"
    );
}

#[test]
fn notfound() {
    let (_dir, store) = tmp_store();
    // Construct a digest that was never stored.
    let d = Digest::of_bytes(b"never stored");
    let err = store.get_bytes(&d).unwrap_err();
    assert!(matches!(err, LightrError::NotFound(_)));
}

// ── materialize ─────────────────────────────────────────────────────────

#[test]
fn materialize_preserves_bytes_and_mode() {
    let (dir, store) = tmp_store();
    let data = b"file content for materialize";
    let d = store.put_bytes(data).unwrap();

    let dest = dir.path().join("out").join("materialized.txt");
    store.materialize_file(&d, &dest, 0o755).unwrap();

    let got = fs::read(&dest).unwrap();
    assert_eq!(&got[..], data);

    // Mode check is unix-only (Windows uses ACLs, not mode bits).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(&dest).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "mode mismatch: got {mode:o}");
    }
}

#[test]
fn materialize_notfound() {
    let (dir, store) = tmp_store();
    let d = Digest::of_bytes(b"not in store");
    let dest = dir.path().join("x");
    let err = store.materialize_file(&d, &dest, 0o644).unwrap_err();
    assert!(matches!(err, LightrError::NotFound(_)));
}

// ── ingest_file ──────────────────────────────────────────────────────────

#[test]
fn ingest_file_roundtrip() {
    let (dir, store) = tmp_store();
    let src = dir.path().join("input.txt");
    let data = b"ingest this file";
    fs::write(&src, data).unwrap();

    let d = store.ingest_file(&src).unwrap();

    // Object must be readable and correct.
    let got = store.get_bytes(&d).unwrap();
    assert_eq!(&got[..], data);
}

#[test]
fn ingest_file_idempotent() {
    let (dir, store) = tmp_store();
    let src = dir.path().join("idem.txt");
    fs::write(&src, b"idempotent ingest").unwrap();

    let d1 = store.ingest_file(&src).unwrap();
    let d2 = store.ingest_file(&src).unwrap();
    assert_eq!(d1, d2);
}

// ── WP-A-dur: fsync path ─────────────────────────────────────────────────

/// fsync path: write → drop store → reopen → content intact.
/// fsync doesn't change observable behavior, but we assert the write+read
/// roundtrip still works and the fsync helper is exercised.
#[test]
fn fsync_put_reopen_roundtrip() {
    let dir = TempDir::new().unwrap();
    let store_path = dir.path().join("store");
    let data = b"durable object data";

    // Write and explicitly drop the store (simulates process restart).
    let digest = {
        let store = Store::open(&store_path).unwrap();
        store.put_bytes(data).unwrap()
    };

    // Reopen the store.
    let store2 = Store::open(&store_path).unwrap();
    let got = store2.get_bytes(&digest).unwrap();
    assert_eq!(&got[..], data);
}

/// Same fsync path via ingest_file.
#[test]
fn fsync_ingest_file_reopen_roundtrip() {
    let dir = TempDir::new().unwrap();
    let store_path = dir.path().join("store");
    let src = dir.path().join("src.txt");
    let data = b"ingest durable data";
    fs::write(&src, data).unwrap();

    let digest = {
        let store = Store::open(&store_path).unwrap();
        store.ingest_file(&src).unwrap()
    };

    let store2 = Store::open(&store_path).unwrap();
    let got = store2.get_bytes(&digest).unwrap();
    assert_eq!(&got[..], data);
}

// ── R1: remove_object ────────────────────────────────────────────────────

#[test]
fn remove_object_removes_and_is_idempotent() {
    let (_dir, store) = tmp_store();
    let data = b"to be removed";
    let d = store.put_bytes(data).unwrap();

    assert!(store.exists(&d), "object must exist after put");
    store.remove_object(&d).unwrap();
    assert!(!store.exists(&d), "object must not exist after remove");

    // Second remove must be Ok(()) — idempotent.
    store.remove_object(&d).unwrap();
}

#[test]
fn remove_object_absent_is_ok() {
    let (_dir, store) = tmp_store();
    let d = Digest::of_bytes(b"never stored");
    // Must succeed without error.
    store.remove_object(&d).unwrap();
}
