//! Tests for `lightr info`. Parallel-safe: each test opens its own tempdir
//! store and drives the pure `gather` core (no process-global env, no daemon).

use super::*;
use lightr_store::Store;

use crate::handlers::testref::{store_with_ref, write_ref};

#[test]
fn empty_store_reports_zeros_and_daemonless() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let info = gather(&store).unwrap();

    assert_eq!(info.images, 0);
    assert_eq!(info.cas_objects, 0);
    assert_eq!(info.cas_bytes, 0);
    assert_eq!(info.build_cache_entries, 0);
    assert!(info.daemonless, "principle #1: always daemonless");
    assert_eq!(info.default_engine, "native");
    assert!(
        info.store_root.contains("store"),
        "store_root surfaces the CAS root path"
    );
}

#[test]
fn info_counts_refs_and_cas_size() {
    let (_tmp, store) = store_with_ref("alpha", b"hello world");
    write_ref(&store, "beta", b"second body here");
    let info = gather(&store).unwrap();

    assert_eq!(info.images, 2, "two named refs ⇒ two images");
    assert!(info.cas_objects >= 2, "at least the two blobs + manifests");
    assert!(
        info.cas_bytes > 0,
        "CAS size sums the reachable object bytes"
    );
    assert!(info.daemonless);
}

#[test]
fn info_json_shape_has_stable_keys() {
    let (_tmp, store) = store_with_ref("gamma", b"x");
    let info = gather(&store).unwrap();
    let s = serde_json::to_string(&info).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();

    for k in [
        "store_root",
        "default_engine",
        "images",
        "cas_objects",
        "cas_bytes",
        "build_cache_entries",
        "daemonless",
    ] {
        assert!(parsed.get(k).is_some(), "missing key {k}");
    }
    assert_eq!(parsed["daemonless"], true);
    assert_eq!(parsed["images"], 1);
}
