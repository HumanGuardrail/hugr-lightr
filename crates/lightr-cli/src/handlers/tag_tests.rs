//! Tests for the `lightr tag` handler — split via `#[path]` (godfile cap).
//!
//! Parallel-safe: each test builds its own tempdir store and writes refs
//! DIRECTLY via the store API (no `lightr_index::snapshot`, whose index cache is
//! keyed off process-global env). The injected core `tag_in_store` is exercised
//! directly.

use super::*;
use lightr_store::Store;

use crate::handlers::testref::store_with_ref;

#[test]
fn tag_aliases_manifest_under_new_name() {
    let (_tmp, store) = store_with_ref("src", b"data");
    let src_root = store.ref_get("src").unwrap().unwrap().root;

    tag_in_store(&store, "src", "alias").unwrap();

    // Both refs now exist and point at the SAME manifest root (zero data copy).
    let dst = store.ref_get("alias").unwrap().expect("alias must exist");
    assert_eq!(
        dst.root, src_root,
        "alias shares the source manifest digest"
    );
    assert!(store.ref_get("src").unwrap().is_some(), "src is unchanged");
}

#[test]
fn tag_then_images_shows_both() {
    let (_tmp, store) = store_with_ref("base", b"data");
    tag_in_store(&store, "base", "copy").unwrap();
    let names = store.list_refs().unwrap();
    assert!(names.contains(&"base".to_string()));
    assert!(names.contains(&"copy".to_string()));
    let rows = lightr_oci::list_images(&store).unwrap();
    assert_eq!(
        rows.len(),
        2,
        "images lists both the original and the alias"
    );
}

#[test]
fn tag_missing_src_is_ref_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let err = tag_in_store(&store, "nosuch", "alias").unwrap_err();
    assert!(
        matches!(err, lightr_core::LightrError::RefNotFound(_)),
        "missing src ⇒ RefNotFound (handler maps to No such image, exit 1)"
    );
}
