//! Tests for the refs plane — split via #[path] to keep refs.rs under the
//! 400-LOC godfile cap (WP-IMG-07 added ref_remove).

use super::*;
use crate::Store;
use lightr_core::LightrError;
use tempfile::TempDir;

fn tmp_store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

fn make_ref_record(name: &str) -> RefRecord {
    RefRecord {
        name: name.to_string(),
        root: Digest::of_bytes(name.as_bytes()),
        parent: None,
        created_at_unix: 1_700_000_000,
        tool_version: "0.1.0".to_string(),
    }
}

// ── refs ─────────────────────────────────────────────────────────────────

#[test]
fn ref_roundtrip() {
    let (_dir, store) = tmp_store();
    let rec = make_ref_record("main");

    store.ref_put(&rec).unwrap();
    let got = store.ref_get("main").unwrap();
    assert!(got.is_some());
    let got = got.unwrap();
    assert_eq!(got.name, rec.name);
    assert_eq!(got.root, rec.root);
    assert_eq!(got.created_at_unix, rec.created_at_unix);
}

#[test]
fn ref_last_write_wins() {
    let (_dir, store) = tmp_store();
    let rec1 = make_ref_record("dev");
    let mut rec2 = make_ref_record("dev");
    rec2.root = Digest::of_bytes(b"second root");

    store.ref_put(&rec1).unwrap();
    store.ref_put(&rec2).unwrap();

    let got = store.ref_get("dev").unwrap().unwrap();
    assert_eq!(got.root, rec2.root, "last-write-wins violated");
}

#[test]
fn ref_absent_returns_none() {
    let (_dir, store) = tmp_store();
    let got = store.ref_get("nonexistent").unwrap();
    assert!(got.is_none());
}

#[test]
fn ref_invalid_name_rejected() {
    let (_dir, store) = tmp_store();
    let rec = RefRecord {
        name: "INVALID NAME WITH SPACES".to_string(),
        root: Digest::of_bytes(b"x"),
        parent: None,
        created_at_unix: 0,
        tool_version: "0.1.0".to_string(),
    };
    let put_err = store.ref_put(&rec).unwrap_err();
    assert!(matches!(put_err, LightrError::InvalidRef(_)));
    let get_err = store.ref_get("INVALID NAME WITH SPACES").unwrap_err();
    assert!(matches!(get_err, LightrError::InvalidRef(_)));
}

// ── R1: ref_log ──────────────────────────────────────────────────────────

#[test]
fn ref_log_three_versions_newest_first() {
    let (_dir, store) = tmp_store();

    let root1 = Digest::of_bytes(b"v1");
    let root2 = Digest::of_bytes(b"v2");
    let root3 = Digest::of_bytes(b"v3");

    let rec1 = RefRecord {
        name: "main".to_string(),
        root: root1,
        parent: None,
        created_at_unix: 1_000,
        tool_version: "0.1.0".to_string(),
    };
    let rec2 = RefRecord {
        name: "main".to_string(),
        root: root2,
        parent: Some(root1),
        created_at_unix: 2_000,
        tool_version: "0.1.0".to_string(),
    };
    let rec3 = RefRecord {
        name: "main".to_string(),
        root: root3,
        parent: Some(root2),
        created_at_unix: 3_000,
        tool_version: "0.1.0".to_string(),
    };

    store.ref_put(&rec1).unwrap();
    store.ref_put(&rec2).unwrap();
    store.ref_put(&rec3).unwrap();

    let log = store.ref_log("main").unwrap();
    assert_eq!(log.len(), 3, "expected 3 log entries");
    // Index 0 = newest (rec3), 1 = rec2, 2 = oldest (rec1).
    assert_eq!(log[0].root, root3, "log[0] must be newest (v3)");
    assert_eq!(log[1].root, root2, "log[1] must be v2");
    assert_eq!(log[2].root, root1, "log[2] must be oldest (v1)");
}

#[test]
fn ref_log_unknown_name_is_empty() {
    let (_dir, store) = tmp_store();
    let log = store.ref_log("does-not-exist").unwrap();
    assert!(log.is_empty(), "unknown ref must return empty log");
}

// R0 LWW still works after R1 extension.
#[test]
fn ref_log_lww_still_works() {
    let (_dir, store) = tmp_store();

    let root1 = Digest::of_bytes(b"first");
    let root2 = Digest::of_bytes(b"second");

    let rec1 = RefRecord {
        name: "dev".to_string(),
        root: root1,
        parent: None,
        created_at_unix: 100,
        tool_version: "0.1.0".to_string(),
    };
    let rec2 = RefRecord {
        name: "dev".to_string(),
        root: root2,
        parent: Some(root1),
        created_at_unix: 200,
        tool_version: "0.1.0".to_string(),
    };

    store.ref_put(&rec1).unwrap();
    store.ref_put(&rec2).unwrap();

    // ref_get must return the LWW (latest).
    let current = store.ref_get("dev").unwrap().unwrap();
    assert_eq!(current.root, root2, "LWW violated after R1 extension");
}

// ── R1: list_refs ─────────────────────────────────────────────────────────

#[test]
fn list_refs_returns_both_names_sorted() {
    let (_dir, store) = tmp_store();

    let rec_b = RefRecord {
        name: "beta".to_string(),
        root: Digest::of_bytes(b"beta"),
        parent: None,
        created_at_unix: 1,
        tool_version: "0.1.0".to_string(),
    };
    let rec_a = RefRecord {
        name: "alpha".to_string(),
        root: Digest::of_bytes(b"alpha"),
        parent: None,
        created_at_unix: 2,
        tool_version: "0.1.0".to_string(),
    };

    store.ref_put(&rec_b).unwrap();
    store.ref_put(&rec_a).unwrap();

    let refs = store.list_refs().unwrap();
    assert_eq!(
        refs,
        vec!["alpha", "beta"],
        "list_refs must be sorted ascending"
    );
}

#[test]
fn list_refs_empty_store() {
    let (_dir, store) = tmp_store();
    assert!(store.list_refs().unwrap().is_empty());
}

// ── WP-IMG-07: ref_remove (untag) ──────────────────────────────────────────

#[test]
fn ref_remove_untags_and_returns_true() {
    let (_dir, store) = tmp_store();
    store.ref_put(&make_ref_record("main")).unwrap();

    let existed = store.ref_remove("main").unwrap();
    assert!(existed, "removing a present ref returns true");
    assert!(
        store.ref_get("main").unwrap().is_none(),
        "removed ref must be gone from ref_get"
    );
    assert!(
        !store.list_refs().unwrap().iter().any(|r| r == "main"),
        "removed ref must vanish from list_refs"
    );
}

#[test]
fn ref_remove_absent_is_false_idempotent() {
    let (_dir, store) = tmp_store();
    assert!(
        !store.ref_remove("never").unwrap(),
        "removing an absent ref returns false (idempotent)"
    );
}

#[test]
fn ref_remove_invalid_name_rejected() {
    let (_dir, store) = tmp_store();
    let err = store.ref_remove("INVALID NAME").unwrap_err();
    assert!(matches!(err, LightrError::InvalidRef(_)));
}

#[test]
fn ref_remove_does_not_disturb_siblings() {
    let (_dir, store) = tmp_store();
    store.ref_put(&make_ref_record("a")).unwrap();
    store.ref_put(&make_ref_record("b")).unwrap();

    store.ref_remove("a").unwrap();
    assert!(store.ref_get("a").unwrap().is_none());
    assert!(
        store.ref_get("b").unwrap().is_some(),
        "removing one ref must not disturb another"
    );
}
