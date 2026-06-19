//! Non-`#[test]` helper functions and types shared across A17–A22 test groups.

use flate2::{write::GzEncoder, Compression};
use sha2::{Digest as Sha2DigestTrait, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

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
pub(crate) fn make_gz_layer(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
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
pub(crate) fn make_oci_layout_real_sha256(dir: &Path, layers: &[Vec<u8>]) -> PathBuf {
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
pub(crate) fn parse_root_from_stdout(stdout: &[u8]) -> String {
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
