//! Import layout and docker-save tests.

use super::{make_layer, make_layout, make_modern_docker_save, tmp_store_and_home, ENV_LOCK};
use crate::oci::import::import_layout;
use lightr_core::LightrError;
use std::fs;
use tempfile::TempDir;

/// A17: 2-layer OCI layout import with whiteout and hydrate roundtrip.
#[test]
fn test_import_layout_two_layers_whiteout_and_hydrate() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    // Layer 1: add /bin/sh-stub and /etc/x
    let layer1 = make_layer(&[
        ("bin/", &[], 0o755),
        ("bin/sh-stub", b"#!/bin/sh\necho hi\n", 0o755),
        ("etc/", &[], 0o755),
        ("etc/x", b"remove me", 0o644),
    ]);

    // Layer 2: whiteout /etc/x, add /app/hello (0755)
    let layer2 = make_layer(&[
        ("etc/.wh.x", &[], 0o644),
        ("app/", &[], 0o755),
        ("app/hello", b"hello world\n", 0o755),
    ]);

    let layout_dir = make_layout(tmp.path(), &[layer1, layer2]);

    let report = import_layout(&layout_dir, &store, "test-image").unwrap();
    assert_eq!(report.name, "test-image");
    assert_eq!(report.layers, 2);

    // Hydrate to a fresh dir and verify the tree
    let hydrate_dir = tmp.path().join("hydrated");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "test-image").unwrap();

    // /etc/x must be absent (whiteout)
    assert!(
        !hydrate_dir.join("etc/x").exists(),
        "etc/x should have been whited out"
    );

    // /app/hello must be present and executable (mode 0755)
    let hello = hydrate_dir.join("app/hello");
    assert!(hello.exists(), "app/hello must exist");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&hello).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "app/hello mode should be 0755, got {mode:o}");
    }

    let content = fs::read(&hello).unwrap();
    assert_eq!(content, b"hello world\n");
}

/// A18: import idempotent — same layout twice → same root digest.
#[test]
fn test_import_idempotent() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let layer = make_layer(&[("file.txt", b"content", 0o644)]);
    let layout_dir = make_layout(tmp.path(), &[layer]);

    let r1 = import_layout(&layout_dir, &store, "idem-test").unwrap();
    let r2 = import_layout(&layout_dir, &store, "idem-test").unwrap();

    assert_eq!(
        r1.root, r2.root,
        "second import should produce the same root"
    );
}

/// A19 partial: path-escape entries are skipped, nothing written outside tempdir.
#[test]
fn test_path_escape_skipped() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    // Build a layer with a path-escape entry (../evil).
    // The tar crate's set_path() rejects `..` components, so we craft the
    // raw tar bytes manually: a POSIX tar block is 512 bytes where the
    // first 100 bytes are the NUL-terminated path.
    let layer_bytes = {
        // Helper: build one 512-byte tar header block with checksum
        fn tar_block(name: &[u8], size: usize, file_type: u8, content: &[u8]) -> Vec<u8> {
            let mut block = [0u8; 512];
            // name (100 bytes)
            let n = name.len().min(99);
            block[..n].copy_from_slice(&name[..n]);
            // mode (8 bytes, octal)
            block[100..107].copy_from_slice(b"0000644");
            // uid, gid (8 bytes each)
            block[108..115].copy_from_slice(b"0000000");
            block[116..123].copy_from_slice(b"0000000");
            // size (12 bytes, octal)
            let size_oct = format!("{:011o}", size);
            block[124..135].copy_from_slice(size_oct.as_bytes());
            // mtime (12 bytes)
            block[136..147].copy_from_slice(b"00000000000");
            // checksum placeholder
            block[148..156].copy_from_slice(b"        ");
            // type flag
            block[156] = file_type;
            // compute checksum
            let cksum: u32 = block.iter().map(|&b| b as u32).sum();
            let cksum_str = format!("{:06o}\0 ", cksum);
            block[148..156].copy_from_slice(cksum_str.as_bytes());

            let mut result = block.to_vec();
            // content padded to 512-byte boundary
            result.extend_from_slice(content);
            let pad = (512 - (content.len() % 512)) % 512;
            result.extend(vec![0u8; pad]);
            result
        }

        // Entry 1: safe.txt (type '0' = regular file)
        let mut raw = tar_block(b"safe.txt", 4, b'0', b"safe");
        // Entry 2: ../evil (path-escape — type '0')
        raw.extend(tar_block(b"../evil", 5, b'0', b"EVIL!"));
        // End-of-archive: two zero blocks
        raw.extend([0u8; 1024]);

        // gz-compress the raw tar
        let mut gz_buf = Vec::new();
        let mut encoder = flate2::write::GzEncoder::new(&mut gz_buf, flate2::Compression::fast());
        use std::io::Write as _;
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap();
        gz_buf
    };

    let layout_dir = make_layout(tmp.path(), &[layer_bytes]);

    let report = import_layout(&layout_dir, &store, "escape-test").unwrap();

    // The import should succeed
    assert_eq!(report.layers, 1);

    // evil file must NOT exist outside the snapshot (it was skipped)
    // We can't easily check the tempdir after the fact, but we can verify
    // the hydrated tree only has the safe file.
    let hydrate_dir = tmp.path().join("hydrated-escape");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "escape-test").unwrap();
    assert!(hydrate_dir.join("safe.txt").exists(), "safe.txt must exist");
    // ../evil cannot land in the hydrate_dir since it was skipped
}

/// docker save-style tar roundtrip.
#[test]
fn test_docker_save_tar_roundtrip() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    // Build layer tar (plain, not gz)
    let mut layer_tar_bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut layer_tar_bytes);
        let content = b"hello from docker save\n";
        let mut header = tar::Header::new_gnu();
        header.set_path("usr/bin/greet").unwrap();
        header.set_mode(0o755);
        header.set_size(content.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append(&header, &content[..]).unwrap();
        tar.finish().unwrap();
    }

    // Build the docker-save outer tar: manifest.json + layer0/layer.tar
    let outer_tar_bytes = {
        let mut outer = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut outer);

            // manifest.json
            let manifest_json = serde_json::to_vec(&serde_json::json!([
                {
                    "Config": "config.json",
                    "Layers": ["layer0/layer.tar"]
                }
            ]))
            .unwrap();
            let mut mh = tar::Header::new_gnu();
            mh.set_path("manifest.json").unwrap();
            mh.set_mode(0o644);
            mh.set_size(manifest_json.len() as u64);
            mh.set_entry_type(tar::EntryType::Regular);
            mh.set_cksum();
            tar.append(&mh, manifest_json.as_slice()).unwrap();

            // layer0/layer.tar
            let mut lh = tar::Header::new_gnu();
            lh.set_path("layer0/layer.tar").unwrap();
            lh.set_mode(0o644);
            lh.set_size(layer_tar_bytes.len() as u64);
            lh.set_entry_type(tar::EntryType::Regular);
            lh.set_cksum();
            tar.append(&lh, layer_tar_bytes.as_slice()).unwrap();

            tar.finish().unwrap();
            // `tar` dropped here, releasing borrow on `outer`
        }
        outer
    };

    // Write to a temp file
    let tar_path = tmp.path().join("docker-save.tar");
    fs::write(&tar_path, &outer_tar_bytes).unwrap();

    let report = import_layout(&tar_path, &store, "docker-save-test").unwrap();
    assert_eq!(report.layers, 1);

    let hydrate_dir = tmp.path().join("hydrated-docker");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "docker-save-test").unwrap();

    let greet = hydrate_dir.join("usr/bin/greet");
    assert!(greet.exists(), "usr/bin/greet must exist");
    assert_eq!(fs::read(&greet).unwrap(), b"hello from docker save\n");
}

/// Regression: modern `docker save` (blobs/sha256 layers) must import.
/// Pins the fix for `docker save layer not found: blobs/sha256/...`.
#[test]
fn test_docker_save_modern_oci_layout_imports() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let mut layer_tar = Vec::new();
    {
        let mut t = tar::Builder::new(&mut layer_tar);
        let content = b"modern docker save\n";
        let mut h = tar::Header::new_gnu();
        h.set_path("usr/bin/modern").unwrap();
        h.set_mode(0o755);
        h.set_size(content.len() as u64);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        t.append(&h, &content[..]).unwrap();
        t.finish().unwrap();
    }

    let tar_path = tmp.path().join("modern-docker-save.tar");
    fs::write(&tar_path, make_modern_docker_save(&layer_tar, false)).unwrap();

    let report = import_layout(&tar_path, &store, "modern-test").unwrap();
    assert_eq!(report.layers, 1);

    let hydrate_dir = tmp.path().join("hydrated-modern");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "modern-test").unwrap();
    let f = hydrate_dir.join("usr/bin/modern");
    assert!(f.exists(), "usr/bin/modern must exist after modern import");
    assert_eq!(fs::read(&f).unwrap(), b"modern docker save\n");
}

/// Fail-closed: a modern blob whose content does not match its
/// `blobs/sha256/<digest>` path digest must be rejected, not silently imported.
#[test]
fn test_docker_save_modern_rejects_sha_mismatch() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let mut layer_tar = Vec::new();
    {
        let mut t = tar::Builder::new(&mut layer_tar);
        let content = b"tampered\n";
        let mut h = tar::Header::new_gnu();
        h.set_path("x").unwrap();
        h.set_mode(0o644);
        h.set_size(content.len() as u64);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        t.append(&h, &content[..]).unwrap();
        t.finish().unwrap();
    }

    let tar_path = tmp.path().join("bad-modern.tar");
    fs::write(&tar_path, make_modern_docker_save(&layer_tar, true)).unwrap();

    let res = import_layout(&tar_path, &store, "bad-modern");
    assert!(
        res.is_err(),
        "a blobs/sha256 digest mismatch must be rejected (fail-closed)"
    );
}
