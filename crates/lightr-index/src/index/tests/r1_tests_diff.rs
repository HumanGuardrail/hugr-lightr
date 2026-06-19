//! `mod r1_tests` — pure diff_manifests + parse_lrr1 tests.
#![cfg(test)]

use crate::index::timeaxis::{diff_manifests, parse_lrr1};
use lightr_core::{Digest, Entry, Manifest};

fn make_manifest(entries: Vec<Entry>) -> Manifest {
    let total_size = entries
        .iter()
        .map(|e| {
            if let Entry::File { size, .. } = e {
                *size
            } else {
                0
            }
        })
        .sum();
    Manifest {
        version: 1,
        total_size,
        entries,
    }
}

fn file(path: &str, digest: [u8; 32], mode: u32, size: u64) -> Entry {
    Entry::File {
        path: path.to_string(),
        digest: Digest(digest),
        mode,
        size,
    }
}

fn symlink(path: &str, target: &str) -> Entry {
    Entry::Symlink {
        path: path.to_string(),
        target: target.to_string(),
    }
}

fn dir(path: &str) -> Entry {
    Entry::Dir {
        path: path.to_string(),
    }
}

// -----------------------------------------------------------------------
// Pure tests — diff_manifests (run now: cargo test -p lightr-index -- diff)
// -----------------------------------------------------------------------

#[test]
fn diff_manifests_identical_empty() {
    let old = make_manifest(vec![]);
    let new = make_manifest(vec![]);
    let r = diff_manifests(&old, &new);
    assert!(r.added.is_empty());
    assert!(r.removed.is_empty());
    assert!(r.changed.is_empty());
}

#[test]
fn diff_manifests_all_added() {
    let old = make_manifest(vec![]);
    let new = make_manifest(vec![
        file("a.txt", [1u8; 32], 0o644, 10),
        file("b.txt", [2u8; 32], 0o644, 20),
    ]);
    let r = diff_manifests(&old, &new);
    assert_eq!(r.added, vec!["a.txt", "b.txt"]);
    assert!(r.removed.is_empty());
    assert!(r.changed.is_empty());
}

#[test]
fn diff_manifests_all_removed() {
    let old = make_manifest(vec![file("x.txt", [3u8; 32], 0o644, 5)]);
    let new = make_manifest(vec![]);
    let r = diff_manifests(&old, &new);
    assert!(r.added.is_empty());
    assert_eq!(r.removed, vec!["x.txt"]);
    assert!(r.changed.is_empty());
}

#[test]
fn diff_manifests_changed_digest() {
    let old = make_manifest(vec![file("f.rs", [0u8; 32], 0o644, 100)]);
    let new = make_manifest(vec![file("f.rs", [1u8; 32], 0o644, 100)]);
    let r = diff_manifests(&old, &new);
    assert!(r.added.is_empty());
    assert!(r.removed.is_empty());
    assert_eq!(r.changed, vec!["f.rs"]);
}

#[test]
fn diff_manifests_changed_mode() {
    let old = make_manifest(vec![file("run.sh", [5u8; 32], 0o644, 50)]);
    let new = make_manifest(vec![file("run.sh", [5u8; 32], 0o755, 50)]);
    let r = diff_manifests(&old, &new);
    assert_eq!(r.changed, vec!["run.sh"]);
}

#[test]
fn diff_manifests_changed_symlink_target() {
    let old = make_manifest(vec![symlink("link", "old_target")]);
    let new = make_manifest(vec![symlink("link", "new_target")]);
    let r = diff_manifests(&old, &new);
    assert_eq!(r.changed, vec!["link"]);
}

#[test]
fn diff_manifests_symlink_unchanged() {
    let old = make_manifest(vec![symlink("link", "same_target")]);
    let new = make_manifest(vec![symlink("link", "same_target")]);
    let r = diff_manifests(&old, &new);
    assert!(r.added.is_empty() && r.removed.is_empty() && r.changed.is_empty());
}

#[test]
fn diff_manifests_changed_kind_file_to_symlink() {
    let old = make_manifest(vec![file("thing", [0u8; 32], 0o644, 10)]);
    let new = make_manifest(vec![symlink("thing", "target")]);
    let r = diff_manifests(&old, &new);
    assert_eq!(r.changed, vec!["thing"]);
}

#[test]
fn diff_manifests_changed_kind_symlink_to_dir() {
    let old = make_manifest(vec![symlink("node", "target")]);
    let new = make_manifest(vec![dir("node")]);
    let r = diff_manifests(&old, &new);
    assert_eq!(r.changed, vec!["node"]);
}

#[test]
fn diff_manifests_mixed_add_remove_change() {
    // old: a (file), b (file), c (file)
    // new: a (changed mode), c (unchanged), d (new)
    let old = make_manifest(vec![
        file("a", [1u8; 32], 0o644, 10),
        file("b", [2u8; 32], 0o644, 20),
        file("c", [3u8; 32], 0o644, 30),
    ]);
    let new = make_manifest(vec![
        file("a", [1u8; 32], 0o755, 10), // mode changed
        file("c", [3u8; 32], 0o644, 30), // unchanged
        file("d", [4u8; 32], 0o644, 40), // new
    ]);
    let r = diff_manifests(&old, &new);
    assert_eq!(r.added, vec!["d"]);
    assert_eq!(r.removed, vec!["b"]);
    assert_eq!(r.changed, vec!["a"]);
}

#[test]
fn diff_manifests_dir_entries_unchanged() {
    let old = make_manifest(vec![dir("empty_dir")]);
    let new = make_manifest(vec![dir("empty_dir")]);
    let r = diff_manifests(&old, &new);
    assert!(r.added.is_empty() && r.removed.is_empty() && r.changed.is_empty());
}

// -----------------------------------------------------------------------
// Pure tests — parse_lrr1 mark-logic
// -----------------------------------------------------------------------

#[test]
fn parse_lrr1_valid() {
    let mut bytes = [0u8; 72];
    bytes[0..4].copy_from_slice(b"LRR1");
    // exit_code = 0 LE at [4..8]
    bytes[4..8].copy_from_slice(&0i32.to_le_bytes());
    // out_digest at [8..40]
    for i in 0..32 {
        bytes[8 + i] = 0xAA;
    }
    // err_digest at [40..72]
    for i in 0..32 {
        bytes[40 + i] = 0xBB;
    }

    let result = parse_lrr1(&bytes);
    assert!(result.is_some());
    let (out_d, err_d) = result.unwrap();
    assert_eq!(out_d.0, [0xAA; 32]);
    assert_eq!(err_d.0, [0xBB; 32]);
}

#[test]
fn parse_lrr1_nonzero_exit_code() {
    let mut bytes = [0u8; 72];
    bytes[0..4].copy_from_slice(b"LRR1");
    bytes[4..8].copy_from_slice(&(-1i32).to_le_bytes());
    for i in 0..32 {
        bytes[8 + i] = 0x11;
    }
    for i in 0..32 {
        bytes[40 + i] = 0x22;
    }

    let result = parse_lrr1(&bytes);
    assert!(result.is_some());
    let (out_d, err_d) = result.unwrap();
    assert_eq!(out_d.0, [0x11; 32]);
    assert_eq!(err_d.0, [0x22; 32]);
}

#[test]
fn parse_lrr1_wrong_magic() {
    let mut bytes = [0u8; 72];
    bytes[0..4].copy_from_slice(b"XXXX");
    assert!(parse_lrr1(&bytes).is_none());
}

#[test]
fn parse_lrr1_wrong_length_short() {
    let bytes = [b'L', b'R', b'R', b'1'];
    assert!(parse_lrr1(&bytes).is_none());
}

#[test]
fn parse_lrr1_wrong_length_long() {
    let bytes = vec![0u8; 73];
    assert!(parse_lrr1(&bytes).is_none());
}

#[test]
fn parse_lrr1_empty() {
    assert!(parse_lrr1(&[]).is_none());
}
