//! Tests for the `lightr history` handler — split via `#[path]` (godfile cap).
//!
//! Parallel-safe: each test builds its own tempdir store and writes refs
//! DIRECTLY via the store API (no `lightr_index::snapshot`, whose index cache is
//! keyed off process-global env). `history` shows the ref VERSION LOG
//! (`ref_log`), newest-first; the pure render helpers are exercised directly.

use super::*;
use lightr_store::Store;

use crate::handlers::testref::write_ref;

#[test]
fn history_shows_ref_log_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();

    // Two versions of the same ref ⇒ two log entries (write_ref chains parent).
    write_ref(&store, "app", b"v1");
    write_ref(&store, "app", b"v2-with-more-bytes");

    let log = store.ref_log("app").unwrap();
    assert_eq!(log.len(), 2, "two snapshots ⇒ two version-log entries");
    // ref_log is newest-first (index 0 = current). The current root must equal
    // the live ref's root.
    let current = store.ref_get("app").unwrap().unwrap();
    assert_eq!(log[0].root, current.root, "log[0] is the current version");
    assert_ne!(log[0].root, log[1].root, "the two versions differ");
}

#[test]
fn history_single_version_one_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    write_ref(&store, "solo", b"only");
    let log = store.ref_log("solo").unwrap();
    assert_eq!(log.len(), 1);
}

#[test]
fn history_short_hex_is_12_chars() {
    let full = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(short_hex(full), "0123456789ab");
}

#[test]
fn history_created_label_honest() {
    assert_eq!(created_label(0), "<unknown>");
    assert_eq!(created_label(1_623_456_000), "2021-06-12");
}

#[test]
fn history_civil_date_epoch() {
    assert_eq!(civil_from_unix_days(0), (1970, 1, 1));
}
