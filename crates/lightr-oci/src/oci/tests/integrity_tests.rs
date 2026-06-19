//! Integrity, whiteout ordering, and hardlink tests.

use super::{make_layer, make_layout, tmp_store_and_home, ENV_LOCK};
use crate::oci::import::import_layout;
use crate::oci::util::{path_is_safe, sha256_hex_of, verify_sha256};
use lightr_core::LightrError;
use std::{fs, path::Path};
use tempfile::TempDir;

#[test]
fn test_path_is_safe() {
    assert!(path_is_safe(Path::new("a/b/c")));
    assert!(path_is_safe(Path::new("./a/b")));
    assert!(!path_is_safe(Path::new("../evil")));
    assert!(!path_is_safe(Path::new("/etc/passwd")));
    assert!(!path_is_safe(Path::new("a/../../etc")));
}

// ── FIX 1: sha256 integrity tests ─────────────────────────────────────────

/// Corrupt a layer blob after writing the layout → import must fail with
/// Integrity error (sha256 mismatch).
#[test]
fn test_integrity_corrupt_layer_fails() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let layer = make_layer(&[("hello.txt", b"hello", 0o644)]);
    let layout_dir = make_layout(tmp.path(), &[layer]);

    // Corrupt one of the layer blobs in blobs/sha256/
    let blobs_dir = layout_dir.join("blobs/sha256");
    let mut entries: Vec<_> = fs::read_dir(&blobs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();
    // The layout has manifest blob + 1 layer blob; corrupt the smaller one
    // that is likely the layer (manifest is JSON, layer is gz tar).
    entries.sort_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(0));
    // Corrupt the layer blob (smallest file, index 0 after sort)
    let corrupt_path = entries[0].path();
    let mut data = fs::read(&corrupt_path).unwrap();
    // Flip a byte in the middle
    let mid = data.len() / 2;
    data[mid] ^= 0xFF;
    fs::write(&corrupt_path, &data).unwrap();

    let result = import_layout(&layout_dir, &store, "corrupt-test");
    assert!(
        matches!(result, Err(LightrError::Integrity { .. })),
        "corrupt blob must produce Integrity error; got: {:?}",
        result.err()
    );
}

/// Verify that `verify_sha256` helper correctly identifies corruption.
#[test]
fn test_verify_sha256_helper() {
    let data = b"test content";
    let good_hex = sha256_hex_of(data);
    assert!(verify_sha256(data, &good_hex).is_ok());

    // Wrong hex → Integrity error
    let bad_hex = "0".repeat(64);
    let err = verify_sha256(data, &bad_hex).unwrap_err();
    assert!(matches!(err, LightrError::Integrity { .. }));
}

// ── FIX 3/4: whiteout ordering tests ─────────────────────────────────────

/// Same-layer add-then-whiteout: the file must be absent (whiteouts win).
#[test]
fn test_intra_layer_whiteout_ordering() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    // Single layer: add x/f AND add x/.wh.f (whiteout of x/f)
    // Per OCI parent-ref semantics our impl documents: whiteouts are
    // processed before additions within a layer, so x/f ends up absent.
    let layer = make_layer(&[
        ("x/", &[], 0o755),
        ("x/f", b"should be absent", 0o644),
        ("x/.wh.f", &[], 0o644), // whiteout of x/f
    ]);

    let layout_dir = make_layout(tmp.path(), &[layer]);
    let report = import_layout(&layout_dir, &store, "wo-order-test").unwrap();
    assert_eq!(report.layers, 1);

    let hydrate_dir = tmp.path().join("hydrated-wo");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "wo-order-test").unwrap();

    assert!(
        !hydrate_dir.join("x/f").exists(),
        "x/f must be absent: whiteout in same layer applies (whiteouts execute before additions)"
    );
}

/// Opaque whiteout clears dir from prior layer; new dir created by opaque.
#[test]
fn test_opaque_whiteout_clears_prior_layer() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    // Layer 1: create dir and file
    let layer1 = make_layer(&[("dir/", &[], 0o755), ("dir/old.txt", b"old", 0o644)]);
    // Layer 2: opaque whiteout of dir, then add a new file in dir
    let layer2 = make_layer(&[
        ("dir/.wh..wh..opq", &[], 0o644), // opaque whiteout
        ("dir/new.txt", b"new", 0o644),
    ]);

    let layout_dir = make_layout(tmp.path(), &[layer1, layer2]);
    import_layout(&layout_dir, &store, "opaque-test").unwrap();

    let hydrate_dir = tmp.path().join("hydrated-opaque");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "opaque-test").unwrap();

    assert!(
        !hydrate_dir.join("dir/old.txt").exists(),
        "dir/old.txt must be absent after opaque whiteout"
    );
    assert!(
        hydrate_dir.join("dir/new.txt").exists(),
        "dir/new.txt must be present after opaque whiteout"
    );
}

// ── FIX 5: hardlink tests ─────────────────────────────────────────────────

/// Hardlink to a present target: both files have identical content.
#[test]
fn test_hardlink_present_target() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    // Build a layer gz with a regular file then a hardlink pointing to it.
    let layer_bytes = {
        let gz_buf = Vec::new();
        let encoder = flate2::write::GzEncoder::new(gz_buf, flate2::Compression::fast());
        let mut tar_b = tar::Builder::new(encoder);

        // Regular file: "original.txt"
        let content = b"link content";
        let mut rh = tar::Header::new_gnu();
        rh.set_path("original.txt").unwrap();
        rh.set_mode(0o644);
        rh.set_size(content.len() as u64);
        rh.set_entry_type(tar::EntryType::Regular);
        rh.set_cksum();
        tar_b.append(&rh, &content[..]).unwrap();

        // Hardlink: "copy.txt" → "original.txt"
        let mut lh = tar::Header::new_gnu();
        lh.set_path("copy.txt").unwrap();
        lh.set_mode(0o644);
        lh.set_size(0);
        lh.set_entry_type(tar::EntryType::Link);
        lh.set_link_name("original.txt").unwrap();
        lh.set_cksum();
        tar_b.append(&lh, &b""[..]).unwrap();

        tar_b.into_inner().unwrap().finish().unwrap()
    };

    let layout_dir = make_layout(tmp.path(), &[layer_bytes]);
    import_layout(&layout_dir, &store, "hardlink-test").unwrap();

    let hydrate_dir = tmp.path().join("hydrated-hl");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "hardlink-test").unwrap();

    let orig = hydrate_dir.join("original.txt");
    let copy = hydrate_dir.join("copy.txt");
    assert!(orig.exists(), "original.txt must exist");
    assert!(copy.exists(), "copy.txt (hardlink) must exist");
    assert_eq!(
        fs::read(&orig).unwrap(),
        fs::read(&copy).unwrap(),
        "hardlinked files must have identical content"
    );
}

/// Dangling hardlink → import must fail (fail-closed).
#[test]
fn test_hardlink_dangling_fails() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let layer_bytes = {
        let gz_buf = Vec::new();
        let encoder = flate2::write::GzEncoder::new(gz_buf, flate2::Compression::fast());
        let mut tar_b = tar::Builder::new(encoder);

        // Hardlink that points to a non-existent target
        let mut lh = tar::Header::new_gnu();
        lh.set_path("dangling.txt").unwrap();
        lh.set_mode(0o644);
        lh.set_size(0);
        lh.set_entry_type(tar::EntryType::Link);
        lh.set_link_name("ghost.txt").unwrap();
        lh.set_cksum();
        tar_b.append(&lh, &b""[..]).unwrap();

        tar_b.into_inner().unwrap().finish().unwrap()
    };

    let layout_dir = make_layout(tmp.path(), &[layer_bytes]);
    let result = import_layout(&layout_dir, &store, "dangling-hl");

    assert!(
        matches!(result, Err(LightrError::InvalidManifest(_))),
        "dangling hardlink must return InvalidManifest; got: {:?}",
        result.err()
    );
    if let Err(LightrError::InvalidManifest(msg)) = result {
        assert!(
            msg.contains("hardlink target not found"),
            "error must mention 'hardlink target not found'; got: {msg}"
        );
    }
}
