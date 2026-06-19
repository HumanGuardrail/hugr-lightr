//! A17 test group: OCI import roundtrip, integrity, whiteout ordering, hardlink.

use super::helpers::*;
use flate2::{write::GzEncoder, Compression};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::common::lightr_cmd;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// A17 — OCI import roundtrip (offline)
// ---------------------------------------------------------------------------

#[test]
fn a17_oci_import_roundtrip() {
    let home = TempDir::new().unwrap();
    let fixture_dir = TempDir::new().unwrap();
    let image_tar = make_oci_layout(fixture_dir.path());

    // Import the docker-save tar.
    let import_out = lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            image_tar.to_str().unwrap(),
            "--name",
            "@t/img",
        ])
        .output()
        .expect("oci import must not fail to launch");
    assert_eq!(
        import_out.status.code().unwrap_or(-1),
        0,
        "oci import must exit 0; stderr: {}",
        String::from_utf8_lossy(&import_out.stderr)
    );

    // Hydrate and verify post-whiteout tree.
    let dest = TempDir::new().unwrap();
    lightr_cmd(home.path())
        .args(["hydrate", dest.path().to_str().unwrap(), "--name", "@t/img"])
        .assert()
        .success();

    // etc/keep must be present with content "k".
    let keep_path = dest.path().join("etc/keep");
    assert!(keep_path.exists(), "etc/keep must be present after hydrate");
    assert_eq!(
        fs::read(&keep_path).unwrap(),
        b"k",
        "etc/keep content must be \"k\""
    );

    // etc/drop must be ABSENT (whiteout applied).
    let drop_path = dest.path().join("etc/drop");
    assert!(
        !drop_path.exists(),
        "etc/drop must be absent (whiteout applied)"
    );

    // bin/tool must be present with mode 0755.
    let tool_path = dest.path().join("bin/tool");
    assert!(tool_path.exists(), "bin/tool must be present after hydrate");
    #[cfg(unix)]
    {
        let tool_mode = fs::metadata(&tool_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            tool_mode, 0o755,
            "bin/tool mode must be 0755; got {:o}",
            tool_mode
        );
    }

    // app/hello must be present with content "hi" and mode 0755.
    let hello_path = dest.path().join("app/hello");
    assert!(
        hello_path.exists(),
        "app/hello must be present after hydrate"
    );
    assert_eq!(
        fs::read(&hello_path).unwrap(),
        b"hi",
        "app/hello content must be \"hi\""
    );
    #[cfg(unix)]
    {
        let hello_mode = fs::metadata(&hello_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            hello_mode, 0o755,
            "app/hello mode must be 0755; got {:o}",
            hello_mode
        );
    }
}

// ---------------------------------------------------------------------------
// a17b — integrity: corrupt layer blob → oci import exits 1, stderr "sha256"
//        or "integrity"; dest never created.
// ---------------------------------------------------------------------------

#[test]
fn a17b_integrity() {
    let home = TempDir::new().unwrap();
    let fixture_dir = TempDir::new().unwrap();

    // Build a valid OCI layout with real sha256.
    let layer = make_gz_layer(&[("hello.txt", b"hello world", 0o644)]);
    let layout_dir = make_oci_layout_real_sha256(fixture_dir.path(), &[layer]);

    // Corrupt one layer blob: find the smallest file in blobs/sha256
    // (layer blob tends to be smaller or same size as manifest; we pick the
    // one whose sha256 hex name is NOT referenced in index.json — i.e. the
    // layer, not the manifest). Simpler: find the blob that is NOT the manifest
    // hex by reading index.json.
    let index_json: serde_json::Value =
        serde_json::from_slice(&fs::read(layout_dir.join("index.json")).unwrap()).unwrap();
    let manifest_hex = index_json["manifests"][0]["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap()
        .to_string();
    let manifest_bytes = fs::read(layout_dir.join("blobs/sha256").join(&manifest_hex)).unwrap();
    let manifest_parsed: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let layer_hex = manifest_parsed["layers"][0]["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap()
        .to_string();

    // Corrupt the layer blob by flipping a byte.
    let layer_path = layout_dir.join("blobs/sha256").join(&layer_hex);
    let mut data = fs::read(&layer_path).unwrap();
    let mid = data.len() / 2;
    data[mid] ^= 0xFF;
    fs::write(&layer_path, &data).unwrap();

    // oci import of the corrupt layout must exit 1 (Integrity → exit 1).
    let out = lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            layout_dir.to_str().unwrap(),
            "--name",
            "@t/corrupt",
        ])
        .output()
        .expect("oci import must not fail to spawn");

    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        1,
        "corrupt OCI layout must exit 1 (integrity error); got exit={} stderr={}",
        code,
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("sha256") || stderr.contains("integrity"),
        "stderr must mention 'sha256' or 'integrity'; got: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The snapshot must NOT have been applied: the ref must not exist.
    // hydrate of @t/corrupt should exit non-zero (ref not found).
    let dest = TempDir::new().unwrap();
    let hydrate_out = lightr_cmd(home.path())
        .args([
            "hydrate",
            dest.path().to_str().unwrap(),
            "--name",
            "@t/corrupt",
        ])
        .output()
        .expect("hydrate must not fail to spawn");
    assert_ne!(
        hydrate_out.status.code().unwrap_or(-1),
        0,
        "hydrate of corrupt import must fail (ref must not have been created)"
    );
}

// ---------------------------------------------------------------------------
// a17c — whiteout ordering: add x/f AND x/.wh.f in the same layer →
//         x/f absent after hydrate (whiteout wins per OCI parent-ref semantics).
// ---------------------------------------------------------------------------

#[test]
fn a17c_whiteout_ordering() {
    let home = TempDir::new().unwrap();
    let fixture_dir = TempDir::new().unwrap();

    // Single layer: add x/f and immediately whiteout x/f in the same layer.
    // Per OCI spec, whiteouts are processed before additions; x/f is absent.
    let layer = make_gz_layer(&[
        ("x/", &[], 0o755),
        ("x/f", b"should be gone", 0o644),
        ("x/.wh.f", &[], 0o644), // whiteout of x/f
    ]);

    let layout_dir = make_oci_layout_real_sha256(fixture_dir.path(), &[layer]);

    let import_out = lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            layout_dir.to_str().unwrap(),
            "--name",
            "@t/wo",
        ])
        .output()
        .expect("oci import must not fail to spawn");
    assert_eq!(
        import_out.status.code().unwrap_or(-1),
        0,
        "oci import must exit 0; stderr: {}",
        String::from_utf8_lossy(&import_out.stderr)
    );

    let dest = TempDir::new().unwrap();
    lightr_cmd(home.path())
        .args(["hydrate", dest.path().to_str().unwrap(), "--name", "@t/wo"])
        .assert()
        .success();

    assert!(
        !dest.path().join("x/f").exists(),
        "x/f must be absent: whiteout in same layer wins (OCI parent-ref semantics)"
    );
}

// ---------------------------------------------------------------------------
// a17d — hardlink: valid hardlink → both files identical content;
//         dangling hardlink → import exits 1.
// ---------------------------------------------------------------------------

#[test]
fn a17d_hardlink() {
    // --- Part 1: valid hardlink ---
    {
        let home = TempDir::new().unwrap();
        let fixture_dir = TempDir::new().unwrap();

        let layer_bytes = {
            let gz_buf = Vec::new();
            let encoder = GzEncoder::new(gz_buf, Compression::fast());
            let mut tar_b = tar::Builder::new(encoder);

            // Regular file: "original.txt"
            let content = b"hardlink content";
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

        let layout_dir = make_oci_layout_real_sha256(fixture_dir.path(), &[layer_bytes]);

        let import_out = lightr_cmd(home.path())
            .args([
                "oci",
                "import",
                layout_dir.to_str().unwrap(),
                "--name",
                "@t/hl",
            ])
            .output()
            .expect("oci import must not fail to spawn");
        assert_eq!(
            import_out.status.code().unwrap_or(-1),
            0,
            "valid hardlink import must exit 0; stderr: {}",
            String::from_utf8_lossy(&import_out.stderr)
        );

        let dest = TempDir::new().unwrap();
        lightr_cmd(home.path())
            .args(["hydrate", dest.path().to_str().unwrap(), "--name", "@t/hl"])
            .assert()
            .success();

        let orig = dest.path().join("original.txt");
        let copy = dest.path().join("copy.txt");
        assert!(
            orig.exists(),
            "original.txt must exist after hardlink hydrate"
        );
        assert!(
            copy.exists(),
            "copy.txt (hardlink) must exist after hydrate"
        );
        assert_eq!(
            fs::read(&orig).unwrap(),
            fs::read(&copy).unwrap(),
            "hardlinked files must have identical content"
        );
    }

    // --- Part 2: dangling hardlink → import exits 1 ---
    {
        let home = TempDir::new().unwrap();
        let fixture_dir = TempDir::new().unwrap();

        let layer_bytes = {
            let gz_buf = Vec::new();
            let encoder = GzEncoder::new(gz_buf, Compression::fast());
            let mut tar_b = tar::Builder::new(encoder);

            // Hardlink to a non-existent target
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

        let layout_dir = make_oci_layout_real_sha256(fixture_dir.path(), &[layer_bytes]);

        let out = lightr_cmd(home.path())
            .args([
                "oci",
                "import",
                layout_dir.to_str().unwrap(),
                "--name",
                "@t/dangling",
            ])
            .output()
            .expect("oci import must not fail to spawn");

        let code = out.status.code().unwrap_or(-1);
        assert_eq!(
            code,
            1,
            "dangling hardlink must exit 1 (InvalidManifest → exit 1); got exit={} stderr={}",
            code,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
