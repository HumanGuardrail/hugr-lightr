// ── Tests (host, runnable — the proof of the pure O(1)-plan + solidifier) ────

use crate::*;
use lightr_core::{Digest, Entry, Manifest};
use std::path::Path;

/// A digest with a recognizable single-byte fill, for stable assertions.
fn digest(fill: u8) -> Digest {
    Digest([fill; 32])
}

/// A multi-kind manifest, intentionally given OUT of path-sorted order so
/// `plan_view` is exercised on its sort guarantee.
fn multi_kind_manifest() -> Manifest {
    let entries = vec![
        Entry::Dir { path: "src".into() },
        Entry::File {
            path: "src/main.rs".into(),
            mode: 0o644,
            size: 10,
            digest: digest(1),
        },
        Entry::Symlink {
            path: "link".into(),
            target: "src/main.rs".into(),
        },
        Entry::File {
            path: "Cargo.toml".into(),
            mode: 0o644,
            size: 20,
            digest: digest(2),
        },
        Entry::File {
            path: "src/lib.rs".into(),
            mode: 0o644,
            size: 30,
            digest: digest(3),
        },
    ];
    Manifest {
        version: 1,
        total_size: 60,
        entries,
    }
}

#[test]
fn plan_view_covers_every_entry_path_sorted() {
    let m = multi_kind_manifest();
    let plan = plan_view(&m);

    // Every entry appears, exactly once.
    assert_eq!(plan.len(), m.entries.len());
    let got: Vec<&str> = plan.entries().iter().map(|e| e.path.as_str()).collect();
    // Path-sorted, regardless of the manifest's input order.
    assert_eq!(
        got,
        vec!["Cargo.toml", "link", "src", "src/lib.rs", "src/main.rs"]
    );

    // Kinds are preserved and files carry their digest; others carry None.
    let by_path = |p: &str| plan.entries().iter().find(|e| e.path == p).unwrap();
    assert_eq!(by_path("Cargo.toml").kind, EntryKind::File);
    assert_eq!(by_path("Cargo.toml").digest, Some(digest(2)));
    assert_eq!(by_path("src").kind, EntryKind::Dir);
    assert_eq!(by_path("src").digest, None);
    assert_eq!(by_path("link").kind, EntryKind::Symlink);
    assert_eq!(by_path("link").digest, None);
    assert_eq!(by_path("src/main.rs").digest, Some(digest(1)));

    // Only the 3 files are promotable.
    assert_eq!(plan.file_count(), 3);
}

#[test]
fn plan_view_handles_empty_manifest() {
    let plan = plan_view(&Manifest {
        version: 1,
        total_size: 0,
        entries: vec![],
    });
    assert!(plan.is_empty());
    assert_eq!(plan.file_count(), 0);
}

#[test]
fn solidifier_seeds_only_files() {
    let plan = plan_view(&multi_kind_manifest());
    let s = Solidifier::new(&plan);
    // 3 files seeded; dirs/symlinks are not promotion candidates.
    assert_eq!(s.files.len(), 3);
    // Nothing promoted yet, so not fully solid.
    assert!(!s.is_fully_solid());
}

#[test]
fn solidifier_promotes_accessed_before_unaccessed_manifest_order_tiebreak() {
    let plan = plan_view(&multi_kind_manifest());
    // Files in manifest (path-sorted) order: Cargo.toml, src/lib.rs, src/main.rs.
    let mut s = Solidifier::new(&plan);

    // Access src/main.rs (last file in manifest order) — it must jump the
    // queue ahead of the un-accessed Cargo.toml / src/lib.rs.
    s.record_access("src/main.rs");

    // Hot entry first.
    assert_eq!(s.next_to_promote().as_deref(), Some("src/main.rs"));
    // Then the rest in manifest order.
    assert_eq!(s.next_to_promote().as_deref(), Some("Cargo.toml"));
    assert_eq!(s.next_to_promote().as_deref(), Some("src/lib.rs"));
    // Nothing left to hand out.
    assert_eq!(s.next_to_promote(), None);
}

#[test]
fn solidifier_multiple_hot_entries_use_manifest_order_within_band() {
    let plan = plan_view(&multi_kind_manifest());
    let mut s = Solidifier::new(&plan);

    // Two hot entries — access the later one first; manifest order still
    // decides within the hot band (deterministic, not access-time-ordered).
    s.record_access("src/main.rs");
    s.record_access("Cargo.toml");

    assert_eq!(s.next_to_promote().as_deref(), Some("Cargo.toml"));
    assert_eq!(s.next_to_promote().as_deref(), Some("src/main.rs"));
    // Then the cold one.
    assert_eq!(s.next_to_promote().as_deref(), Some("src/lib.rs"));
    assert_eq!(s.next_to_promote(), None);
}

#[test]
fn record_access_is_idempotent() {
    let plan = plan_view(&multi_kind_manifest());
    let mut s = Solidifier::new(&plan);

    s.record_access("src/lib.rs");
    s.record_access("src/lib.rs");
    s.record_access("src/lib.rs");

    // Still exactly one hand-out for it, still first (it's the only hot one).
    assert_eq!(s.next_to_promote().as_deref(), Some("src/lib.rs"));
    assert_eq!(s.next_to_promote().as_deref(), Some("Cargo.toml"));
    assert_eq!(s.next_to_promote().as_deref(), Some("src/main.rs"));
    assert_eq!(s.next_to_promote(), None);
}

#[test]
fn record_access_on_nonfile_or_unknown_is_ignored() {
    let plan = plan_view(&multi_kind_manifest());
    let mut s = Solidifier::new(&plan);

    // A dir, a symlink, and an unknown path — none is a promotable file.
    s.record_access("src"); // dir
    s.record_access("link"); // symlink
    s.record_access("does/not/exist");

    // No file got hot, so plain manifest order applies.
    assert_eq!(s.next_to_promote().as_deref(), Some("Cargo.toml"));
    assert_eq!(s.next_to_promote().as_deref(), Some("src/lib.rs"));
    assert_eq!(s.next_to_promote().as_deref(), Some("src/main.rs"));
}

#[test]
fn is_fully_solid_flips_only_after_all_files_promoted() {
    let plan = plan_view(&multi_kind_manifest());
    let mut s = Solidifier::new(&plan);

    // Hand out + confirm them one at a time; solid only at the very end.
    while let Some(p) = s.next_to_promote() {
        assert!(!s.is_fully_solid(), "must not be solid mid-flight");
        s.mark_promoted(&p);
    }
    assert!(s.is_fully_solid(), "solid once all files confirmed");
}

#[test]
fn handing_out_without_confirming_is_not_solid() {
    let plan = plan_view(&multi_kind_manifest());
    let mut s = Solidifier::new(&plan);

    // Drain next_to_promote (all handed out) but confirm none.
    while s.next_to_promote().is_some() {}
    assert_eq!(s.next_to_promote(), None);
    // Handed out != promoted: a clone could still fail, so NOT solid.
    assert!(!s.is_fully_solid());

    // Confirm all but one — still not solid.
    s.mark_promoted("Cargo.toml");
    s.mark_promoted("src/lib.rs");
    assert!(!s.is_fully_solid());
    s.mark_promoted("src/main.rs");
    assert!(s.is_fully_solid());
}

#[test]
fn empty_view_is_trivially_fully_solid() {
    let plan = plan_view(&Manifest {
        version: 1,
        total_size: 0,
        entries: vec![],
    });
    let mut s = Solidifier::new(&plan);
    assert!(s.is_fully_solid());
    assert_eq!(s.next_to_promote(), None);
}

#[test]
fn fake_backend_records_mount_fault_in_unmount() {
    let plan = plan_view(&multi_kind_manifest());
    let mut be = FakeBackend::new();
    let at = Path::new("/tmp/view-root");

    be.mount(&plan, at).unwrap();
    be.fault_in("Cargo.toml").unwrap();
    be.fault_in("src/main.rs").unwrap();
    // Idempotent on the faulted set: the call is recorded, the set isn't dup'd.
    be.fault_in("Cargo.toml").unwrap();
    be.unmount(at).unwrap();

    assert_eq!(be.mounted, vec![at.to_path_buf()]);
    assert_eq!(be.unmounted, vec![at.to_path_buf()]);
    assert_eq!(
        be.fault_calls,
        vec![
            "Cargo.toml".to_string(),
            "src/main.rs".to_string(),
            "Cargo.toml".to_string()
        ]
    );
    // Distinct faulted set deduped.
    assert_eq!(be.faulted.len(), 2);
    assert!(be.faulted.contains("Cargo.toml"));
    assert!(be.faulted.contains("src/main.rs"));
}

#[test]
fn solidify_step_drives_promotion_in_order_with_fake_backend() {
    use std::collections::HashMap;

    // Real store (lightr-store is real and host-testable). Ingest content,
    // build a manifest whose digests point at the ingested objects.
    let tmp = tempfile::tempdir().unwrap();
    let store = lightr_store::Store::open(tmp.path()).unwrap();

    let contents: HashMap<&str, &[u8]> = HashMap::from([
        ("Cargo.toml", b"[package]" as &[u8]),
        ("src/lib.rs", b"pub fn lib() {}" as &[u8]),
        ("src/main.rs", b"fn main() {}" as &[u8]),
    ]);

    let d_cargo = store.put_bytes(contents["Cargo.toml"]).unwrap();
    let d_lib = store.put_bytes(contents["src/lib.rs"]).unwrap();
    let d_main = store.put_bytes(contents["src/main.rs"]).unwrap();

    let manifest = Manifest {
        version: 1,
        total_size: 0,
        entries: vec![
            Entry::Dir { path: "src".into() },
            Entry::File {
                path: "Cargo.toml".into(),
                mode: 0o644,
                size: contents["Cargo.toml"].len() as u64,
                digest: d_cargo,
            },
            Entry::File {
                path: "src/lib.rs".into(),
                mode: 0o644,
                size: contents["src/lib.rs"].len() as u64,
                digest: d_lib,
            },
            Entry::File {
                path: "src/main.rs".into(),
                mode: 0o644,
                size: contents["src/main.rs"].len() as u64,
                digest: d_main,
            },
        ],
    };

    let plan = plan_view(&manifest);
    let mut be = FakeBackend::new();
    let mut sol = Solidifier::new(&plan);
    let dest_root = tmp.path().join("view");

    // Make src/main.rs hot so it promotes first (out of manifest order).
    sol.record_access("src/main.rs");

    // Drive every step; collect the order the driver promotes in.
    let mut order = Vec::new();
    while let Some(p) = solidify_step(&plan, &mut be, &mut sol, &store, &dest_root).unwrap() {
        order.push(p);
    }

    // Hot first, then manifest order — the solidifier's documented policy,
    // observed end-to-end through the driver.
    assert_eq!(order, vec!["src/main.rs", "Cargo.toml", "src/lib.rs"]);

    // The driver faulted each promoted entry into the backend, in order.
    assert_eq!(be.fault_calls, order);

    // It CoW-cloned real content to disk for each file (store path proven).
    for (rel, bytes) in &contents {
        let landed = std::fs::read(dest_root.join(rel)).unwrap();
        assert_eq!(&landed, bytes, "content for {rel} must match the store");
    }

    // Fully solid, mount can evaporate.
    assert!(sol.is_fully_solid());
}
