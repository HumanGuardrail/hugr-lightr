//! Tests for the `lightr images` handler — split via `#[path]` to keep
//! images.rs under the 400-line godfile cap (house convention).
//!
//! Parallel-safe: each test builds its own tempdir store and writes refs
//! DIRECTLY via the store API (no `lightr_index::snapshot`, whose index cache is
//! keyed off process-global env and would race under the parallel runner). The
//! pure helpers (`civil_from_unix_days`, `human_size`, `created_label`) are
//! exercised directly; the listing/rows are asserted against `lightr_oci`'s
//! sizing core that the handler composes.

use super::*;
use lightr_store::Store;

use crate::handlers::testref::store_with_ref;

// ── civil date math ───────────────────────────────────────────────────────────

#[test]
fn civil_from_unix_days_known_dates() {
    // 1970-01-01 = day 0.
    assert_eq!(civil_from_unix_days(0), (1970, 1, 1));
    // 2000-01-01 = day 10957.
    assert_eq!(civil_from_unix_days(10_957), (2000, 1, 1));
    // 2021-06-12 (a date used elsewhere in the suite).
    let (y, m, d) = civil_from_unix_days(1_623_456_000 / 86_400);
    assert_eq!((y, m), (2021, 6));
    assert!((11..=12).contains(&d));
}

#[test]
fn created_label_zero_is_unknown() {
    assert_eq!(created_label(0), "<unknown>");
}

#[test]
fn created_label_formats_iso_date() {
    // 2021-06-12 00:00:00 UTC = 1623456000.
    assert_eq!(created_label(1_623_456_000), "2021-06-12");
}

// ── human_size (docker base-1000) ─────────────────────────────────────────────

#[test]
fn human_size_units() {
    assert_eq!(human_size(0), "0B");
    assert_eq!(human_size(999), "999B");
    assert_eq!(human_size(1_000), "1.0KB");
    assert_eq!(human_size(4_200_000), "4.2MB");
}

// ── listing semantics (composed lightr_oci core) ──────────────────────────────

#[test]
fn images_lists_named_ref_with_size_and_id() {
    let (_tmp, store) = store_with_ref("alpha", b"hello world");
    let rows = lightr_oci::list_images(&store).unwrap();
    assert_eq!(rows.len(), 1, "one ref ⇒ one row");
    let row = &rows[0];
    assert_eq!(row.repository, "alpha");
    assert_eq!(row.tag, NONE_TAG, "no ':' ⇒ <none> tag");
    assert_eq!(row.image_id.len(), 12, "IMAGE ID is 12-char short hex");
    assert!(
        row.size > 0,
        "size sums reachable objects (manifest + blob)"
    );
    // The handler's NONE_TAG sentinel must match lightr_oci's.
    assert_eq!(NONE_TAG, "<none>");
}

#[test]
fn images_created_comes_from_ref_record() {
    let (_tmp, store) = store_with_ref("beta", b"x");
    let rec = store.ref_get("beta").unwrap().unwrap();
    // The ref was just written ⇒ a non-zero, post-epoch timestamp ⇒ a real date.
    assert!(rec.created_at_unix > 0);
    let label = created_label(rec.created_at_unix);
    assert_ne!(label, "<unknown>");
    assert_eq!(label.len(), 10, "YYYY-MM-DD");
}

#[test]
fn images_empty_store_lists_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let rows = lightr_oci::list_images(&store).unwrap();
    assert!(rows.is_empty());
}
