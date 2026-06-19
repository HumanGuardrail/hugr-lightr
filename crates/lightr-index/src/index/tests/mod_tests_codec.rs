//! `mod tests` — index codec + entries_differ tests (tests 7–10).
#![cfg(test)]

use crate::index::codec::{index_path_for, Index};
use crate::index::scan::scan;
use crate::index::status::entries_differ;
use lightr_core::{Digest, Entry};
use std::fs;
use tempfile::TempDir;

use super::super::super::TEST_ENV_LOCK;

fn with_lightr_home(tmp: &TempDir) -> std::sync::MutexGuard<'static, ()> {
    let guard = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("LIGHTR_HOME", tmp.path());
    guard
}

// -----------------------------------------------------------------------
// 7. index file path derivation
// -----------------------------------------------------------------------
#[test]
fn test_index_path_for_is_deterministic() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);
    let root = TempDir::new().unwrap();
    let p1 = index_path_for(root.path()).unwrap();
    let p2 = index_path_for(root.path()).unwrap();
    assert_eq!(p1, p2);
    // Must be under LIGHTR_HOME/index/
    assert!(p1.starts_with(home.path().join("index")));
}

// -----------------------------------------------------------------------
// 8. Index encode/decode round-trip
// -----------------------------------------------------------------------
#[test]
fn test_index_encode_decode_roundtrip() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);
    let root = TempDir::new().unwrap();
    let rp = root.path();

    fs::write(rp.join("hello.txt"), b"world").unwrap();

    let mut idx = Index::empty();
    let _report = scan(rp, &mut idx).unwrap();

    // save
    idx.save_for(rp).unwrap();

    // load
    let idx2 = Index::load_for(rp).unwrap();
    assert_eq!(idx2.entries.len(), idx.entries.len());
    assert_eq!(idx2.entries[0].path, idx.entries[0].path);
    assert_eq!(idx2.entries[0].digest.0, idx.entries[0].digest.0);
}

// -----------------------------------------------------------------------
// 9. Corrupt index treated as empty
// -----------------------------------------------------------------------
#[test]
fn test_corrupt_index_treated_as_empty() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);
    let root = TempDir::new().unwrap();
    let rp = root.path();
    fs::write(rp.join("x.txt"), b"x").unwrap();

    let mut idx = Index::empty();
    scan(rp, &mut idx).unwrap();
    idx.save_for(rp).unwrap();

    // Corrupt the index file
    let ipath = index_path_for(rp).unwrap();
    fs::write(&ipath, b"GARBAGE DATA NOT AN INDEX").unwrap();

    let idx3 = Index::load_for(rp).unwrap();
    assert!(
        idx3.entries.is_empty(),
        "corrupt index should load as empty"
    );
}

// -----------------------------------------------------------------------
// 10. entries_differ logic
// -----------------------------------------------------------------------
#[test]
fn test_entries_differ() {
    let d1 = Digest([1u8; 32]);
    let d2 = Digest([2u8; 32]);
    let f1 = Entry::File {
        path: "a".into(),
        mode: 0o644,
        size: 10,
        digest: d1,
    };
    let f2 = Entry::File {
        path: "a".into(),
        mode: 0o644,
        size: 10,
        digest: d1,
    };
    let f3 = Entry::File {
        path: "a".into(),
        mode: 0o755,
        size: 10,
        digest: d1,
    };
    let f4 = Entry::File {
        path: "a".into(),
        mode: 0o644,
        size: 10,
        digest: d2,
    };
    assert!(
        !entries_differ(&f1, &f2),
        "identical entries should not differ"
    );
    assert!(entries_differ(&f1, &f3), "mode change should differ");
    assert!(entries_differ(&f1, &f4), "digest change should differ");

    let s1 = Entry::Symlink {
        path: "s".into(),
        target: "t1".into(),
    };
    let s2 = Entry::Symlink {
        path: "s".into(),
        target: "t1".into(),
    };
    let s3 = Entry::Symlink {
        path: "s".into(),
        target: "t2".into(),
    };
    assert!(!entries_differ(&s1, &s2));
    assert!(entries_differ(&s1, &s3));

    let dir1 = Entry::Dir { path: "d".into() };
    let dir2 = Entry::Dir { path: "d".into() };
    assert!(!entries_differ(&dir1, &dir2));

    // Different kinds
    assert!(entries_differ(&f1, &s1));
    assert!(entries_differ(&f1, &dir1));
}
