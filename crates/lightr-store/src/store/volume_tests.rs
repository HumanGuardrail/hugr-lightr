//! Tests for the named-volume registry (WP-VOL-4). Split out of `volume.rs` to
//! keep each file under the 400-LOC house limit. Included as a `#[path]` child
//! module of `volume`, so `super::*` resolves to the registry API. Parallel-safe:
//! each test owns a unique tempdir root (atomic counter + nanos) — NO global env.

use super::*;
use lightr_core::LightrError;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;

// Unique per-test root (atomic counter + nanos) — NO global env, parallel-safe.
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_root() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = dir.path().join(format!("store-{n}-{nanos}"));
    fs::create_dir_all(&root).unwrap();
    (dir, root)
}

#[test]
fn create_makes_data_dir_and_meta() {
    let (_d, root) = tmp_root();
    let info = create(&root, "data", &[]).unwrap();
    assert_eq!(info.name, "data");
    assert_eq!(info.driver, "local");
    assert!(data_dir(&root, "data").is_dir(), "_data/ must exist");
    assert!(meta_path(&root, "data").is_file(), "meta.json must exist");
    assert_eq!(info.mountpoint, data_dir(&root, "data"));
}

#[test]
fn create_with_labels_roundtrips_sorted() {
    let (_d, root) = tmp_root();
    let labels = vec![
        ("zone".to_string(), "b".to_string()),
        ("app".to_string(), "web".to_string()),
    ];
    create(&root, "vol", &labels).unwrap();
    let got = inspect(&root, "vol").unwrap();
    assert_eq!(
        got.labels,
        vec![
            ("app".to_string(), "web".to_string()),
            ("zone".to_string(), "b".to_string()),
        ],
        "labels must roundtrip sorted by key"
    );
}

#[test]
fn create_existing_errors() {
    let (_d, root) = tmp_root();
    create(&root, "dup", &[]).unwrap();
    let err = create(&root, "dup", &[]).unwrap_err();
    assert!(matches!(err, LightrError::InvalidRef(_)));
    assert!(err.to_string().contains("already exists"));
}

#[test]
fn create_bad_name_rejected() {
    let (_d, root) = tmp_root();
    for bad in ["", "has space", "-leading", "bad/slash", "tab\tname"] {
        let err = create(&root, bad, &[]).unwrap_err();
        assert!(
            matches!(err, LightrError::InvalidRef(_)),
            "name {bad:?} must be rejected"
        );
    }
    // No partial dir left behind for the invalid names.
    assert!(list(&root).unwrap().is_empty());
}

#[test]
fn list_sorted_and_empty() {
    let (_d, root) = tmp_root();
    assert!(
        list(&root).unwrap().is_empty(),
        "empty registry ⇒ empty list"
    );
    create(&root, "beta", &[]).unwrap();
    create(&root, "alpha", &[]).unwrap();
    let names: Vec<String> = list(&root).unwrap().into_iter().map(|v| v.name).collect();
    assert_eq!(names, vec!["alpha", "beta"], "list must be sorted by name");
}

#[test]
fn inspect_missing_errors() {
    let (_d, root) = tmp_root();
    let err = inspect(&root, "ghost").unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}

#[test]
fn inspect_json_has_fields() {
    let (_d, root) = tmp_root();
    create(&root, "j", &[("k".to_string(), "v".to_string())]).unwrap();
    let json = inspect(&root, "j").unwrap().to_json();
    assert!(json.contains("\"name\":\"j\""), "json: {json}");
    assert!(json.contains("\"driver\":\"local\""), "json: {json}");
    assert!(json.contains("\"mountpoint\":"), "json: {json}");
    assert!(json.contains("\"k\":\"v\""), "json: {json}");
}

#[test]
fn remove_then_gone() {
    let (_d, root) = tmp_root();
    create(&root, "tmp", &[]).unwrap();
    remove(&root, "tmp", false).unwrap();
    assert!(!volume_dir(&root, "tmp").exists());
    assert!(inspect(&root, "tmp").is_err());
}

#[test]
fn remove_missing_errors() {
    let (_d, root) = tmp_root();
    let err = remove(&root, "nope", false).unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}

#[test]
fn remove_in_use_refused() {
    let (_d, root) = tmp_root();
    create(&root, "busy", &[]).unwrap();
    let err = remove(&root, "busy", true).unwrap_err();
    assert!(matches!(err, LightrError::InvalidRef(_)));
    assert!(err.to_string().contains("in use"));
    // Still present — refusal must not delete.
    assert!(volume_dir(&root, "busy").exists());
}

#[test]
fn prune_removes_dangling() {
    let (_d, root) = tmp_root();
    create(&root, "a", &[]).unwrap();
    create(&root, "b", &[]).unwrap();
    let removed = prune(&root).unwrap();
    assert_eq!(
        removed,
        vec!["a", "b"],
        "prune returns removed names sorted"
    );
    assert!(list(&root).unwrap().is_empty(), "all dangling removed");
}

#[test]
fn prune_empty_registry_ok() {
    let (_d, root) = tmp_root();
    assert!(prune(&root).unwrap().is_empty());
}

#[test]
fn meta_json_escapes_label_values() {
    let (_d, root) = tmp_root();
    create(&root, "esc", &[("note".to_string(), "a\"b\\c".to_string())]).unwrap();
    let got = inspect(&root, "esc").unwrap();
    assert_eq!(
        got.labels,
        vec![("note".to_string(), "a\"b\\c".to_string())],
        "escaped quote/backslash must roundtrip"
    );
}
