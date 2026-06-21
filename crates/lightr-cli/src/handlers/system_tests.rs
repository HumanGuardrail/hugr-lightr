//! Tests for `lightr system df` / `system prune`. Parallel-safe: each test
//! opens its own tempdir store; the gc-reuse path is exercised directly on that
//! store (no process-global env, no default-store side effects).

use super::*;
use lightr_index::gc;
use lightr_store::Store;

use crate::handlers::testref::{store_with_ref, write_ref};

// ── human_size (docker base-1000) ─────────────────────────────────────────────

#[test]
fn human_size_units() {
    assert_eq!(human_size(0), "0B");
    assert_eq!(human_size(999), "999B");
    assert_eq!(human_size(1_000), "1.0KB");
    assert_eq!(human_size(4_200_000), "4.2MB");
}

// ── system df ─────────────────────────────────────────────────────────────────

#[test]
fn df_empty_store_two_zero_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let report = gather_df(&store).unwrap();
    assert_eq!(report.rows.len(), 2, "Images + Build Cache rows");
    assert_eq!(report.rows[0].kind, "Images");
    assert_eq!(report.rows[0].total, 0);
    assert_eq!(report.rows[0].size, 0);
    assert_eq!(report.rows[1].kind, "Build Cache");
    assert_eq!(report.rows[1].total, 0);
}

#[test]
fn df_images_row_counts_refs_and_size() {
    let (_tmp, store) = store_with_ref("alpha", b"hello world");
    write_ref(&store, "beta", b"another blob");
    let report = gather_df(&store).unwrap();
    let images = &report.rows[0];
    assert_eq!(images.kind, "Images");
    assert_eq!(images.total, 2, "two refs");
    assert!(images.size > 0, "size sums CAS object bytes");
}

#[test]
fn df_build_cache_row_counts_ac_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    // Write two AC entries directly (build cache = the Action Cache).
    let k1 = store.put_bytes(b"key-one-material").unwrap();
    let k2 = store.put_bytes(b"key-two-material").unwrap();
    store.ac_put(&k1, b"AC-VALUE-ONE").unwrap();
    store.ac_put(&k2, b"AC-VALUE-TWO-LONGER").unwrap();

    let report = gather_df(&store).unwrap();
    let bc = &report.rows[1];
    assert_eq!(bc.kind, "Build Cache");
    assert_eq!(bc.total, 2, "two AC entries");
    assert_eq!(
        bc.size,
        (b"AC-VALUE-ONE".len() + b"AC-VALUE-TWO-LONGER".len()) as u64
    );
}

#[test]
fn df_json_shape_has_stable_keys() {
    let (_tmp, store) = store_with_ref("gamma", b"x");
    let report = gather_df(&store).unwrap();
    let s = serde_json::to_string(&report).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    let rows = parsed["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        for k in ["kind", "total", "size", "reclaimable_objects"] {
            assert!(row.get(k).is_some(), "missing df key {k}");
        }
    }
}

// ── system prune (reuses gc) ──────────────────────────────────────────────────

#[test]
fn prune_reuses_gc_and_never_untags_refs() {
    // A tagged ref's objects are reachable ⇒ gc (and thus prune) must NOT sweep
    // them, and the ref must survive. This is the same gc the prune handler calls.
    let (_tmp, store) = store_with_ref("keep-me", b"reachable bytes");
    let before = store.list_refs().unwrap().len();

    // force=true ⇒ dry_run=false; min_age 0 so anything unreachable is eligible.
    let report = gc(&store, false, 0).unwrap();

    // No unreachable objects exist (everything is ref-reachable) ⇒ nothing swept.
    assert_eq!(report.swept, 0, "prune never reaps ref-reachable objects");
    // The ref is still tagged after the sweep (Docker prune keeps tagged images).
    let after = store.list_refs().unwrap();
    assert_eq!(after.len(), before, "ref count unchanged");
    assert!(after.contains(&"keep-me".to_string()), "ref still tagged");
}

#[test]
fn prune_reclaims_unreachable_objects() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    // A loose object with no ref/AC/manifest referencing it ⇒ unreachable.
    store.put_bytes(b"orphan blob nobody references").unwrap();

    // Dry-run first: reports the reclaimable COUNT without deleting (gc's
    // dry-run does not sum bytes — that is what `system df` surfaces as the
    // reclaimable-object count).
    let dry = gc(&store, true, 0).unwrap();
    assert_eq!(dry.swept, 1, "one unreachable object previewed");
    assert_eq!(dry.bytes_freed, 0, "dry-run does not sum bytes");
    // The object survives the preview (nothing deleted).
    assert_eq!(store.store_usage().unwrap().objects, 1);

    // Force sweep actually reclaims it (the prune --force path): bytes summed.
    let done = gc(&store, false, 0).unwrap();
    assert_eq!(done.swept, 1, "one unreachable object reclaimed");
    assert!(done.bytes_freed > 0, "Total reclaimed space > 0");
    assert_eq!(store.store_usage().unwrap().objects, 0, "object gone");
}
