//! Tests for the read-only CAS usage walk (WP-EDGE-VERBS).
//! Parallel-safe: every test opens its own tempdir store, no global state.

use super::*;
use crate::Store;

#[test]
fn empty_store_is_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let u = store_usage(store.root()).unwrap();
    assert_eq!(
        u,
        StoreUsage {
            objects: 0,
            bytes: 0
        }
    );
}

#[test]
fn missing_objects_dir_is_zero() {
    let tmp = tempfile::tempdir().unwrap();
    // Point at a root with no objects/ subtree at all.
    let u = store_usage(&tmp.path().join("nope")).unwrap();
    assert_eq!(u, StoreUsage::default());
}

#[test]
fn counts_objects_and_sums_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let a = b"hello world"; // 11 bytes
    let b = b"lightr"; //      6 bytes
    store.put_bytes(a).unwrap();
    store.put_bytes(b).unwrap();
    let u = store_usage(store.root()).unwrap();
    assert_eq!(u.objects, 2, "two distinct blobs ⇒ two objects");
    assert_eq!(u.bytes, (a.len() + b.len()) as u64);
}

#[test]
fn dedup_does_not_double_count() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    store.put_bytes(b"same").unwrap();
    store.put_bytes(b"same").unwrap(); // idempotent: no new object
    let u = store_usage(store.root()).unwrap();
    assert_eq!(u.objects, 1);
    assert_eq!(u.bytes, 4);
}

#[test]
fn stray_non_conforming_files_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    store.put_bytes(b"real").unwrap();
    // A stray file whose name does not match the 2/62 shard shape.
    let objects = store.root().join("objects");
    std::fs::write(objects.join("README"), b"ignore me").unwrap();
    let u = store_usage(store.root()).unwrap();
    assert_eq!(u.objects, 1, "stray top-level file is not an object");
    assert_eq!(u.bytes, 4);
}
