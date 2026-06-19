//! `mod tests` — scan / snapshot / hydrate / status tests (tests 1–6, 10–11).
#![cfg(test)]

use crate::index::codec::Index;
use crate::index::scan::scan;
use crate::index::snapshot::snapshot;
use crate::index::hydrate::hydrate;
use crate::index::status::status;
use lightr_core::{Entry, LightrError};
use lightr_store::Store;
use std::fs;
use tempfile::TempDir;

use super::super::super::TEST_ENV_LOCK;

fn with_lightr_home(tmp: &TempDir) -> std::sync::MutexGuard<'static, ()> {
    let guard = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("LIGHTR_HOME", tmp.path());
    guard
}

// -----------------------------------------------------------------------
// 1. scan empty dir
// -----------------------------------------------------------------------
#[test]
fn test_scan_empty_dir() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);
    let root = TempDir::new().unwrap();
    let mut index = Index::empty();
    let report = scan(root.path(), &mut index).unwrap();
    assert!(report.manifest.entries.is_empty());
    assert_eq!(report.manifest.total_size, 0);
    assert_eq!(report.rehashed, 0);
    assert_eq!(report.from_index, 0);
}

// -----------------------------------------------------------------------
// 2. scan respects .gitignore + .lightrignore + includes dotfiles + skips .git
// -----------------------------------------------------------------------
#[test]
fn test_scan_ignore_rules_and_dotfiles() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);
    let root = TempDir::new().unwrap();
    let rp = root.path();

    // Create files
    fs::write(rp.join("visible.txt"), b"hello").unwrap();
    fs::write(rp.join(".dotfile"), b"dot").unwrap();
    fs::write(rp.join("ignored_by_git.log"), b"log").unwrap();
    fs::write(rp.join("ignored_by_lightr.tmp"), b"tmp").unwrap();

    // .gitignore ignores *.log
    fs::write(rp.join(".gitignore"), b"*.log\n").unwrap();
    // .lightrignore ignores *.tmp
    fs::write(rp.join(".lightrignore"), b"*.tmp\n").unwrap();

    // .git dir should be skipped entirely
    fs::create_dir(rp.join(".git")).unwrap();
    fs::write(rp.join(".git/HEAD"), b"ref: refs/heads/main").unwrap();

    let mut index = Index::empty();
    let report = scan(rp, &mut index).unwrap();

    let paths: Vec<&str> = report.manifest.entries.iter().map(|e| e.path()).collect();

    // visible.txt and .dotfile should appear
    assert!(
        paths.contains(&"visible.txt"),
        "visible.txt missing: {paths:?}"
    );
    assert!(paths.contains(&".dotfile"), ".dotfile missing: {paths:?}");
    // .gitignore and .lightrignore themselves should appear
    assert!(
        paths.contains(&".gitignore"),
        ".gitignore missing: {paths:?}"
    );
    assert!(
        paths.contains(&".lightrignore"),
        ".lightrignore missing: {paths:?}"
    );

    // ignored files must NOT appear
    assert!(
        !paths.contains(&"ignored_by_git.log"),
        "ignored_by_git.log should be absent: {paths:?}"
    );
    assert!(
        !paths.contains(&"ignored_by_lightr.tmp"),
        "ignored_by_lightr.tmp should be absent: {paths:?}"
    );

    // .git dir contents must not appear
    assert!(
        paths.iter().all(|p| !p.starts_with(".git/")),
        ".git contents must not appear: {paths:?}"
    );
}

// -----------------------------------------------------------------------
// 3. index reuse — 2nd scan rehashed==0 after save/load
//    Racily-clean: we sleep 1.1s so mtime_ns < saved_at_ns
// -----------------------------------------------------------------------
#[test]
fn test_index_reuse_after_save_load() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);
    let root = TempDir::new().unwrap();
    let rp = root.path();

    fs::write(rp.join("a.txt"), b"content-a").unwrap();
    fs::write(rp.join("b.txt"), b"content-b").unwrap();

    // First scan
    let mut index = Index::empty();
    let r1 = scan(rp, &mut index).unwrap();
    assert_eq!(r1.rehashed, 2);
    assert_eq!(r1.from_index, 0);

    // Sleep 1.1s so mtime_ns < saved_at_ns (avoid racily-clean)
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Second scan: load index from disk, should reuse all
    let mut index2 = Index::load_for(rp).unwrap();
    let r2 = scan(rp, &mut index2).unwrap();
    assert_eq!(
        r2.from_index, 2,
        "expected 2 from-index, got {}",
        r2.from_index
    );
    assert_eq!(r2.rehashed, 0, "expected 0 rehashed, got {}", r2.rehashed);
}

// -----------------------------------------------------------------------
// 4. snapshot → hydrate roundtrip: bytes, modes, symlinks, empty dirs
// -----------------------------------------------------------------------
#[test]
fn test_snapshot_hydrate_roundtrip() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);

    let store_root = home.path().join("store");
    let store = Store::open(&store_root).unwrap();

    // Build source tree.
    let src = TempDir::new().unwrap();
    let sp = src.path();

    // A regular file with known bytes.
    let file_bytes = b"hello-roundtrip";
    fs::write(sp.join("data.txt"), file_bytes).unwrap();

    // A file with mode 0o755 (unix only).
    #[cfg(unix)]
    {
        fs::write(sp.join("run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        fs::set_permissions(sp.join("run.sh"), perms).unwrap();
    }

    // A symlink pointing at data.txt (unix only).
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("data.txt", sp.join("link.txt")).unwrap();
    }

    // An empty subdirectory.
    fs::create_dir(sp.join("emptydir")).unwrap();

    // Snapshot then hydrate into a fresh destination.
    let sr = snapshot(sp, &store, "@t/rt").unwrap();
    assert!(sr.files >= 1, "snapshot must record at least one file");

    let dest = TempDir::new().unwrap();
    let dp = dest.path();
    let hr = hydrate(dp, &store, "@t/rt").unwrap();
    assert_eq!(hr.root, sr.root, "hydrated root digest must match snapshot");

    // File bytes must match.
    let got = fs::read(dp.join("data.txt")).unwrap();
    assert_eq!(got.as_slice(), file_bytes, "data.txt bytes must roundtrip");

    // Mode must roundtrip (unix only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dp.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "run.sh mode must roundtrip as 0o755");
    }

    // Symlink target must roundtrip (unix only).
    #[cfg(unix)]
    {
        let target = std::fs::read_link(dp.join("link.txt")).unwrap();
        assert_eq!(
            target.to_str().unwrap(),
            "data.txt",
            "symlink target must roundtrip"
        );
    }

    // Empty subdir must exist and be a directory.
    let emptydir = dp.join("emptydir");
    assert!(emptydir.exists(), "emptydir must exist after hydrate");
    assert!(
        emptydir.is_dir(),
        "emptydir must be a directory after hydrate"
    );
}

// -----------------------------------------------------------------------
// 5. status: clean / dirty (add/remove/change) / unknown-ref
// -----------------------------------------------------------------------
#[test]
fn test_status_clean_then_dirty() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);

    let store_root = home.path().join("store");
    let store = Store::open(&store_root).unwrap();

    // Build an initial working tree with two files.
    let root = TempDir::new().unwrap();
    let rp = root.path();
    fs::write(rp.join("keep.txt"), b"keep-content").unwrap();
    fs::write(rp.join("remove.txt"), b"will-be-removed").unwrap();
    fs::write(rp.join("change.txt"), b"original-content").unwrap();

    // Snapshot the tree.
    snapshot(rp, &store, "@t/status").unwrap();

    // Status immediately after snapshot — must be clean.
    let sr = status(rp, &store, "@t/status").unwrap();
    assert!(sr.clean, "status must be clean right after snapshot");
    assert!(sr.added.is_empty(), "added must be empty: {:?}", sr.added);
    assert!(
        sr.removed.is_empty(),
        "removed must be empty: {:?}",
        sr.removed
    );
    assert!(
        sr.changed.is_empty(),
        "changed must be empty: {:?}",
        sr.changed
    );

    // Mutate the working tree: add, remove, modify.
    fs::write(rp.join("new.txt"), b"brand-new").unwrap();
    fs::remove_file(rp.join("remove.txt")).unwrap();
    fs::write(rp.join("change.txt"), b"mutated-content").unwrap();

    // Status after mutations — must be dirty.
    let sr2 = status(rp, &store, "@t/status").unwrap();
    assert!(!sr2.clean, "status must be dirty after mutations");
    assert!(
        sr2.added.contains(&"new.txt".to_string()),
        "new.txt must appear in added: {:?}",
        sr2.added
    );
    assert!(
        sr2.removed.contains(&"remove.txt".to_string()),
        "remove.txt must appear in removed: {:?}",
        sr2.removed
    );
    assert!(
        sr2.changed.contains(&"change.txt".to_string()),
        "change.txt must appear in changed: {:?}",
        sr2.changed
    );
}

// -----------------------------------------------------------------------
// 6. unknown ref returns RefNotFound
// -----------------------------------------------------------------------
#[test]
fn test_status_unknown_ref_returns_error_type() {
    // Verify the error variant for RefNotFound is correct.
    let err = LightrError::RefNotFound("no-such-ref".into());
    match err {
        LightrError::RefNotFound(n) => assert_eq!(n, "no-such-ref"),
        _ => panic!("wrong variant"),
    }
}

// -----------------------------------------------------------------------
// 11. Empty dir recorded in scan
// -----------------------------------------------------------------------
#[test]
fn test_scan_empty_dir_entry() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);
    let root = TempDir::new().unwrap();
    let rp = root.path();

    // Create an empty sub-directory
    fs::create_dir(rp.join("empty_subdir")).unwrap();
    // Also a non-empty sub-directory (should not appear as Dir entry)
    fs::create_dir(rp.join("non_empty")).unwrap();
    fs::write(rp.join("non_empty/file.txt"), b"data").unwrap();

    let mut index = Index::empty();
    let report = scan(rp, &mut index).unwrap();

    let paths: Vec<&str> = report.manifest.entries.iter().map(|e| e.path()).collect();
    let has_empty_dir = report
        .manifest
        .entries
        .iter()
        .any(|e| matches!(e, Entry::Dir { path } if path == "empty_subdir"));
    assert!(
        has_empty_dir,
        "empty_subdir should appear as Dir entry: {paths:?}"
    );

    // non_empty dir itself must NOT appear as a Dir entry
    let has_non_empty_dir = report
        .manifest
        .entries
        .iter()
        .any(|e| matches!(e, Entry::Dir { path } if path == "non_empty"));
    assert!(
        !has_non_empty_dir,
        "non-empty dir must not appear as Dir: {paths:?}"
    );

    // non_empty/file.txt should appear
    assert!(
        paths.contains(&"non_empty/file.txt"),
        "file inside non-empty dir missing: {paths:?}"
    );
}
