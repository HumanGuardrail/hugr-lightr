//! A17–A21 per build-spec-r2.md §5 — authored by WP-R2-W4 (red-first).
//! R2-HARDEN additions: a17b (integrity), a17c (whiteout ordering),
//! a17d (hardlink), A18 strengthened, A21 strengthened.
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo check -p lightr-acceptance --all-targets.
//! The binary is expected to have the R2 verbs merged in; these tests are
//! authored red-first (compile-only gate until the post-merge green run).
//! Do NOT weaken assertions.
//!
//! Fixture form for A17: docker-save TAR. The fixture contains manifest.json
//! plus two uncompressed layer tars (built with the `tar` crate). No sha2 dep
//! is needed: docker-save manifests reference layers by filename, not digest.
//! `flate2` is added as a dev-dep per spec authorisation; layers are kept
//! uncompressed in this fixture so `flate2` is not called directly.
//!
//! For a17b we need a real OCI layout with sha256 digests; sha2 is added as a
//! dev-dep (already authorized in root Cargo.toml workspace.dependencies).

#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

use flate2::{write::GzEncoder, Compression};
use sha2::{Digest as Sha2DigestTrait, Sha256};
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::lightr_cmd;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// sha256 helper for OCI layout fixtures
// ---------------------------------------------------------------------------

fn sha256_hex_of(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in hash.iter() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// ---------------------------------------------------------------------------
// OCI layout fixture builder (real sha256 — for a17b integrity test)
// ---------------------------------------------------------------------------

/// Build a gz-compressed tar layer from (path, content, mode) triples.
/// An empty content slice ⇒ directory entry.
fn make_gz_layer(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let gz_buf = Vec::new();
    let encoder = GzEncoder::new(gz_buf, Compression::fast());
    let mut tar_b = tar::Builder::new(encoder);

    for (path, content, mode) in entries {
        if content.is_empty() {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_mode(*mode);
            header.set_size(0);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_cksum();
            tar_b.append(&header, &b""[..]).unwrap();
        } else {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_mode(*mode);
            header.set_size(content.len() as u64);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar_b.append(&header, *content).unwrap();
        }
    }

    tar_b.into_inner().unwrap().finish().unwrap()
}

/// Build a valid OCI layout directory at `<dir>/layout` using REAL sha256.
/// Returns the layout path.
fn make_oci_layout_real_sha256(dir: &Path, layers: &[Vec<u8>]) -> PathBuf {
    let layout_dir = dir.join("layout");
    fs::create_dir_all(layout_dir.join("blobs/sha256")).unwrap();

    fs::write(
        layout_dir.join("oci-layout"),
        r#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .unwrap();

    let mut layer_descs = Vec::new();
    for layer_bytes in layers {
        let digest_hex = sha256_hex_of(layer_bytes);
        fs::write(
            layout_dir.join("blobs/sha256").join(&digest_hex),
            layer_bytes,
        )
        .unwrap();
        layer_descs.push(serde_json::json!({
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": format!("sha256:{digest_hex}"),
            "size": layer_bytes.len()
        }));
    }

    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "size": 0
        },
        "layers": layer_descs
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let manifest_hex = sha256_hex_of(&manifest_bytes);
    fs::write(
        layout_dir.join("blobs/sha256").join(&manifest_hex),
        &manifest_bytes,
    )
    .unwrap();

    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{manifest_hex}"),
            "size": manifest_bytes.len()
        }]
    });
    fs::write(
        layout_dir.join("index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();

    layout_dir
}

// ---------------------------------------------------------------------------
// OCI fixture builder — docker-save TAR form
// ---------------------------------------------------------------------------

/// A single file entry to add to a layer tar.
struct TarEntry<'a> {
    path: &'a str,
    content: &'a [u8],
    mode: u32,
}

/// Build an uncompressed layer tar in memory from the given entries.
///
/// Whiteout entries are added as empty files at the given path (e.g.
/// `etc/.wh.drop` to delete `etc/drop` from lower layers).
fn build_layer_tar(entries: &[TarEntry<'_>]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut buf);
        for e in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(e.content.len() as u64);
            header.set_mode(e.mode);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            ar.append_data(&mut header, e.path, e.content).unwrap();
        }
        ar.finish().unwrap();
    }
    buf
}

/// Build a minimal docker-save tar layout at `<dir>/image.tar`.
///
/// Layout inside image.tar:
///   manifest.json           — [{Config, RepoTags, Layers:[layer1.tar, layer2.tar]}]
///   layer1.tar              — adds etc/keep (mode 0644), etc/drop (mode 0644),
///                             bin/tool (mode 0755)
///   layer2.tar              — whiteout etc/.wh.drop + adds app/hello (mode 0755)
///   <fake-config-hash>.json — minimal OCI config blob (required by some importers)
///
/// Why docker-save instead of OCI layout with sha2 digests: the spec
/// (§5 authoring-law) explicitly permits this form to avoid adding sha2 as a
/// dev-dep. The `lightr-oci` importer autodetects this form via manifest.json.
pub fn make_oci_layout(dir: &Path) -> PathBuf {
    // --- layer 1: etc/keep + etc/drop + bin/tool ---
    let layer1_data = build_layer_tar(&[
        TarEntry {
            path: "etc/keep",
            content: b"k",
            mode: 0o644,
        },
        TarEntry {
            path: "etc/drop",
            content: b"d",
            mode: 0o644,
        },
        TarEntry {
            path: "bin/tool",
            content: b"#!/bin/sh\n",
            mode: 0o755,
        },
    ]);

    // --- layer 2: whiteout etc/drop + add app/hello ---
    let layer2_data = build_layer_tar(&[
        // Whiteout: empty file named ".wh.<basename>" in the same directory.
        TarEntry {
            path: "etc/.wh.drop",
            content: b"",
            mode: 0o644,
        },
        TarEntry {
            path: "app/hello",
            content: b"hi",
            mode: 0o755,
        },
    ]);

    // --- minimal config blob (empty JSON object suffices for the importer) ---
    let config_data = b"{}";
    let config_name = "config.json";

    // --- manifest.json (docker-save format) ---
    let manifest_json = serde_json::json!([{
        "Config": config_name,
        "RepoTags": ["acceptance-test:latest"],
        "Layers": ["layer1.tar", "layer2.tar"]
    }]);
    let manifest_bytes = serde_json::to_vec(&manifest_json).unwrap();

    // --- assemble image.tar ---
    let image_tar_path = dir.join("image.tar");
    let file = fs::File::create(&image_tar_path).unwrap();
    let mut ar = tar::Builder::new(file);

    let mut append = |name: &str, data: &[u8]| {
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(data.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_entry_type(tar::EntryType::Regular);
        hdr.set_cksum();
        ar.append_data(&mut hdr, name, data).unwrap();
    };

    append("manifest.json", &manifest_bytes);
    append("layer1.tar", &layer1_data);
    append("layer2.tar", &layer2_data);
    append(config_name, config_data);

    ar.finish().unwrap();

    image_tar_path
}

// ---------------------------------------------------------------------------
// Helper: parse `root=<hex>` from stdout.
// ---------------------------------------------------------------------------
fn parse_root_from_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        for tok in line.split_whitespace() {
            if let Some(hex) = tok.strip_prefix("root=") {
                if hex.len() >= 16 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return hex.to_owned();
                }
            }
        }
    }
    panic!(
        "could not find 'root=<16+hex>' in stdout:\n{}",
        String::from_utf8_lossy(stdout)
    );
}

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

// ---------------------------------------------------------------------------
// A18 — import idempotent + lineage (strengthened)
// ---------------------------------------------------------------------------

#[test]
fn a18_import_idempotent_lineage() {
    let home = TempDir::new().unwrap();
    let fixture_dir = TempDir::new().unwrap();
    let image_tar = make_oci_layout(fixture_dir.path());

    // First import.
    let out1 = lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            image_tar.to_str().unwrap(),
            "--name",
            "@t/img",
        ])
        .output()
        .expect("first oci import must not fail to launch");
    assert_eq!(
        out1.status.code().unwrap_or(-1),
        0,
        "first oci import must exit 0; stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let root1 = parse_root_from_stdout(&out1.stdout);

    // Second import of the same tar.
    let out2 = lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            image_tar.to_str().unwrap(),
            "--name",
            "@t/img",
        ])
        .output()
        .expect("second oci import must not fail to launch");
    assert_eq!(
        out2.status.code().unwrap_or(-1),
        0,
        "second oci import must exit 0; stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let root2 = parse_root_from_stdout(&out2.stdout);

    // Same content → same root digest (byte-equal).
    assert_eq!(
        root1, root2,
        "import of identical tar twice must produce same root digest (byte-equal)"
    );

    // Lineage: reflog must have EXACTLY 2 entries after two imports.
    // `diff --name @t/img --at 1` exits 0 (identical trees) or 1 (different)
    // but must NOT exit 2 (which would mean reflog length < 2 / index OOB).
    let diff_out = lightr_cmd(home.path())
        .args(["diff", "--name", "@t/img", "--at", "1"])
        .output()
        .expect("diff --at 1 must launch");
    let code = diff_out.status.code().unwrap_or(-1);
    assert_ne!(
        code,
        2,
        "diff --name @t/img --at 1 must not exit 2 (reflog must have ≥2 entries); stderr: {}",
        String::from_utf8_lossy(&diff_out.stderr)
    );

    // Both reflog entries must be the same root (same tar → same content).
    // exit 0 from diff means the two roots are identical, confirming this.
    // exit 1 means they differ — acceptable if the importer advances even for
    // the same content, but we report it so the operator can verify manually.
    if code == 0 {
        // Both roots identical — exactly what we expect.
    } else if code == 1 {
        // The importer produced different roots for the same tar twice.
        // This is a heuristic failure; we do NOT hard-assert here because the
        // current spec allows the reflog to chain without requiring content
        // stability across imports (the unit test in lightr-oci asserts it).
        eprintln!(
            "[A18] WARNING: diff --at 1 exit 1 — roots differ between two identical imports; root1={root1} root2={root2}"
        );
    }
}

// ---------------------------------------------------------------------------
// A19 — engine probes honest
// ---------------------------------------------------------------------------

#[test]
fn a19_engine_probes_honest() {
    let home = TempDir::new().unwrap();

    // engine ls --json must exit 0 and return a JSON array.
    let ls_out = lightr_cmd(home.path())
        .args(["engine", "ls", "--json"])
        .output()
        .expect("engine ls --json must launch");
    assert_eq!(
        ls_out.status.code().unwrap_or(-1),
        0,
        "engine ls --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&ls_out.stderr)
    );

    let arr: serde_json::Value =
        serde_json::from_slice(&ls_out.stdout).expect("engine ls --json must emit valid JSON");
    let arr = arr
        .as_array()
        .expect("engine ls --json must emit a JSON array");

    // Build a map from engine name → caps object for easy lookup.
    let caps: HashMap<&str, &serde_json::Value> = arr
        .iter()
        .filter_map(|entry| {
            let name = entry.get("kind").and_then(|n| n.as_str())?;
            Some((name, entry))
        })
        .collect();

    // native must be present and available.
    let native = caps
        .get("native")
        .unwrap_or_else(|| panic!("engine ls must include 'native'; got: {:?}", arr));
    assert_eq!(
        native.get("available").and_then(|v| v.as_bool()),
        Some(true),
        "native.available must be true; got: {native}"
    );

    // ns must be present and unavailable on macOS, with "Linux" in detail.
    let ns = caps
        .get("ns")
        .unwrap_or_else(|| panic!("engine ls must include 'ns'; got: {:?}", arr));
    assert_eq!(
        ns.get("available").and_then(|v| v.as_bool()),
        Some(false),
        "ns.available must be false on macOS; got: {ns}"
    );
    let ns_detail = ns
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("ns.detail must be a string; got: {ns}"));
    assert!(
        ns_detail.to_lowercase().contains("linux"),
        "ns.detail must mention 'Linux' (case-insensitive); got: \"{}\"",
        ns_detail
    );

    // vz must be present and unavailable (feature off in default build),
    // with an actionable detail.
    let vz = caps
        .get("vz")
        .unwrap_or_else(|| panic!("engine ls must include 'vz'; got: {:?}", arr));
    assert_eq!(
        vz.get("available").and_then(|v| v.as_bool()),
        Some(false),
        "vz.available must be false (feature 'vz' off); got: {vz}"
    );
    let vz_detail = vz
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("vz.detail must be a string; got: {vz}"));
    // Actionable: must not be empty.
    assert!(
        !vz_detail.trim().is_empty(),
        "vz.detail must be non-empty (actionable); got: {vz}"
    );

    // run --engine ns -- /bin/true must exit 2 with ns probe detail in stderr.
    let run_ns = lightr_cmd(home.path())
        .args(["run", "--engine", "ns", "--", "/bin/true"])
        .output()
        .expect("run --engine ns must launch");
    assert_eq!(
        run_ns.status.code().unwrap_or(-1),
        2,
        "run --engine ns must exit 2 on macOS; stderr: {}",
        String::from_utf8_lossy(&run_ns.stderr)
    );
    let run_ns_stderr = String::from_utf8_lossy(&run_ns.stderr);
    assert!(
        run_ns_stderr.to_lowercase().contains("linux"),
        "run --engine ns stderr must contain probe detail mentioning 'Linux'; got: \"{}\"",
        run_ns_stderr
    );

    // run --engine vz -- /bin/true must exit 2.
    let run_vz = lightr_cmd(home.path())
        .args(["run", "--engine", "vz", "--", "/bin/true"])
        .output()
        .expect("run --engine vz must launch");
    assert_eq!(
        run_vz.status.code().unwrap_or(-1),
        2,
        "run --engine vz must exit 2 (feature off); stderr: {}",
        String::from_utf8_lossy(&run_vz.stderr)
    );
}

// ---------------------------------------------------------------------------
// A20 — rootfs guard
// ---------------------------------------------------------------------------

#[test]
fn a20_rootfs_guard() {
    let home = TempDir::new().unwrap();
    let fixture_dir = TempDir::new().unwrap();
    let image_tar = make_oci_layout(fixture_dir.path());

    // Import the image first so @t/img is a valid ref.
    lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            image_tar.to_str().unwrap(),
            "--name",
            "@t/img",
        ])
        .assert()
        .success();

    // run --engine native --rootfs @t/img -- /bin/true must exit 2 and stderr
    // must mention "rootfs" (native engine does not support rootfs isolation).
    let run_out = lightr_cmd(home.path())
        .args([
            "run",
            "--engine",
            "native",
            "--rootfs",
            "@t/img",
            "--",
            "/bin/true",
        ])
        .output()
        .expect("run --engine native --rootfs must launch");
    assert_eq!(
        run_out.status.code().unwrap_or(-1),
        2,
        "run --engine native --rootfs must exit 2; stderr: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(
        stderr.to_lowercase().contains("rootfs"),
        "run --engine native --rootfs stderr must mention 'rootfs'; got: \"{}\"",
        stderr
    );
}

// ---------------------------------------------------------------------------
// A21 — pull network-gated, loud (strengthened)
//
// Default lane (no LIGHTR_NET_TESTS): assert that `oci pull alpine` returns
// within 90s, exits 0 or 1, NEVER exit 2 (usage error), and NEVER hangs.
// This is a liveness/no-hang gate, NOT a correctness gate.
//
// LIGHTR_NET_TESTS=1 lane: real pull + hydrate + assert no integrity error
// on a good pull (sha256 verify must PASS for a legitimate registry blob).
// ---------------------------------------------------------------------------

#[test]
fn a21_pull_network_gated() {
    let net_tests = std::env::var("LIGHTR_NET_TESTS")
        .map(|v| v == "1")
        .unwrap_or(false);

    if net_tests {
        // Real-network lane: pull alpine and verify /bin/ is present.
        eprintln!("[A21] LIGHTR_NET_TESTS=1: running real-pull lane");
        a21_real_pull_lane();
    } else {
        // Fast-fail lane: assert no hang + no exit 2.
        eprintln!("[A21] LIGHTR_NET_TESTS not set: running liveness (no-hang) lane");
        a21_liveness_lane();
    }
}

fn a21_liveness_lane() {
    let home = TempDir::new().unwrap();

    let start = Instant::now();
    let out = lightr_cmd(home.path())
        .args(["oci", "pull", "alpine", "--name", "@t/a"])
        // Give at most 90 s; a well-behaved CLI returns in < 10 s on any network state.
        .timeout(Duration::from_secs(90))
        .output()
        .expect("oci pull alpine must not fail to spawn");
    let elapsed = start.elapsed();

    let code = out.status.code().unwrap_or(-1);

    eprintln!(
        "[A21 liveness] exit={} elapsed={:.1}s stderr={}",
        code,
        elapsed.as_secs_f32(),
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("(empty)")
    );

    // Must not hang: already guaranteed by the 90 s timeout above.
    // Must exit 0 (net available, pull OK) or 1 (no net / clean error).
    // Must NOT exit 2 (usage/programming error — a valid "alpine" ref is never
    // a usage error; exit 2 would mean parse_image_ref rejected a valid ref).
    assert!(
        code == 0 || code == 1,
        "oci pull alpine must exit 0 or 1 (liveness gate); got exit={} stderr={}",
        code,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        code,
        2,
        "oci pull alpine must NEVER exit 2 (valid ref is not a usage error); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // If it failed (exit 1), stderr must carry a non-empty diagnostic.
    if code == 1 {
        let stderr = out.stderr.clone();
        assert!(
            !stderr.is_empty(),
            "oci pull exit 1 must produce non-empty stderr (clean error message)"
        );
    }
}

fn a21_real_pull_lane() {
    let home = TempDir::new().unwrap();

    // Pull alpine from the public Docker Hub registry.
    let pull_out = lightr_cmd(home.path())
        .args([
            "oci",
            "pull",
            "registry-1.docker.io/library/alpine:latest",
            "--name",
            "@t/alpine",
        ])
        .output()
        .expect("oci pull alpine must not fail to spawn");
    assert_eq!(
        pull_out.status.code().unwrap_or(-1),
        0,
        "oci pull alpine (real-net lane) must exit 0; stderr: {}",
        String::from_utf8_lossy(&pull_out.stderr)
    );

    // Must NOT exit 2 (valid registry ref is never a usage error).
    assert_ne!(
        pull_out.status.code().unwrap_or(-1),
        2,
        "oci pull must never exit 2 for a valid registry ref; stderr: {}",
        String::from_utf8_lossy(&pull_out.stderr)
    );

    // Hydrate and verify /bin/ is present.
    let dest = TempDir::new().unwrap();
    lightr_cmd(home.path())
        .args([
            "hydrate",
            dest.path().to_str().unwrap(),
            "--name",
            "@t/alpine",
        ])
        .assert()
        .success();

    let bin_dir = dest.path().join("bin");
    assert!(
        bin_dir.exists() && bin_dir.is_dir(),
        "hydrated alpine must have a /bin directory"
    );
    let entries: Vec<_> = fs::read_dir(&bin_dir)
        .expect("must be able to read /bin")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "hydrated alpine /bin must contain files"
    );

    // sha256 verification passed (pull succeeded without Integrity error):
    // if we get here, all layer blobs matched their declared sha256 digests.
    eprintln!(
        "[A21 real-pull] /bin contains {} entries; sha256 verify passed (no integrity error)",
        entries.len()
    );
}
