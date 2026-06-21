//! Tests for the `lightr rmi` handler — split via `#[path]` (godfile cap).
//!
//! Parallel-safe: each test builds its own tempdir store and writes refs
//! DIRECTLY via the store API (no `lightr_index::snapshot`, which keys its index
//! cache off the process-global `LIGHTR_HOME`/`HOME` env and would race under the
//! parallel runner). The injected core `rmi_in_store` is exercised directly.

use super::*;
use lightr_store::Store;

use crate::handlers::testref::store_with_ref;

#[test]
fn rmi_removes_existing_ref_exit_0() {
    let (_tmp, store) = store_with_ref("gone", b"data");
    assert!(store.ref_get("gone").unwrap().is_some());
    let code = rmi_in_store(&store, &["gone".to_string()]);
    assert_eq!(code, 0, "removing an existing image ⇒ exit 0");
    assert!(
        store.ref_get("gone").unwrap().is_none(),
        "ref must be untagged (removed) after rmi"
    );
}

#[test]
fn rmi_missing_is_exit_1() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let code = rmi_in_store(&store, &["ghost".to_string()]);
    assert_eq!(
        code, 1,
        "missing image ⇒ No such image, exit 1 (docker rmi)"
    );
}

#[test]
fn rmi_continue_on_error_worst_wins() {
    let (_tmp, store) = store_with_ref("real", b"data");
    // One present, one absent ⇒ present removed, absent ⇒ exit 1 overall.
    let code = rmi_in_store(&store, &["real".to_string(), "nope".to_string()]);
    assert_eq!(code, 1);
    assert!(
        store.ref_get("real").unwrap().is_none(),
        "present one removed"
    );
}

#[test]
fn rmi_is_idempotent_after_removal() {
    let (_tmp, store) = store_with_ref("once", b"data");
    assert_eq!(rmi_in_store(&store, &["once".to_string()]), 0);
    // Second rmi of the same ref ⇒ now missing ⇒ exit 1.
    assert_eq!(rmi_in_store(&store, &["once".to_string()]), 1);
}
