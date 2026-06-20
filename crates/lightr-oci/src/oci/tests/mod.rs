//! Test suite for lightr-oci. Split across submodules to keep each file <400 LOC.

mod http_tests;
mod import_tests;
mod integrity_tests;
mod pull_tests;
mod push_tests;
mod retain_tests;

use crate::oci::util::sha256_hex_of;
use flate2::{write::GzEncoder, Compression};
use lightr_store::Store;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

// ── Serialization lock: snapshot/hydrate touch LIGHTR_HOME ───────────────
pub(super) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn tmp_store_and_home() -> (TempDir, Store) {
    let home = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", home.path());
    let store = Store::open(home.path().join("store")).unwrap();
    (home, store)
}

// ── Fixture helpers ───────────────────────────────────────────────────────

/// Build a gz-compressed tar layer from (path, content, mode) triples.
/// An empty content vec ⇒ directory entry.
pub(super) fn make_layer(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let gz_buf = Vec::new();
    let encoder = GzEncoder::new(gz_buf, Compression::fast());
    let mut tar = tar::Builder::new(encoder);

    for (path, content, mode) in entries {
        if content.is_empty() {
            // directory
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_mode(*mode);
            header.set_size(0);
            header.set_entry_type(tar::EntryType::Directory);
            header.set_cksum();
            tar.append(&header, &b""[..]).unwrap();
        } else {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_mode(*mode);
            header.set_size(content.len() as u64);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append(&header, *content).unwrap();
        }
    }

    let encoder = tar.into_inner().unwrap();
    encoder.finish().unwrap()
}

/// Write a minimal valid OCI layout into `dir` using REAL sha256 digests:
///   - oci-layout
///   - blobs/sha256/<manifest-hex>  (the manifest JSON)
///   - blobs/sha256/<layer0-hex>    (first layer)
///   - ...
///   - index.json
///
/// Returns the layout directory path.
pub(super) fn make_layout(dir: &Path, layers: &[Vec<u8>]) -> PathBuf {
    let layout_dir = dir.join("layout");
    fs::create_dir_all(layout_dir.join("blobs/sha256")).unwrap();

    // Write oci-layout marker
    fs::write(
        layout_dir.join("oci-layout"),
        r#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .unwrap();

    // Write layer blobs and collect descriptors using REAL sha256.
    let mut layer_descs = Vec::new();
    for layer_bytes in layers {
        let digest_hex = sha256_hex_of(layer_bytes);
        let blob_path = layout_dir.join("blobs/sha256").join(&digest_hex);
        fs::write(&blob_path, layer_bytes).unwrap();
        layer_descs.push(serde_json::json!({
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": format!("sha256:{digest_hex}"),
            "size": layer_bytes.len()
        }));
    }

    // Write manifest using REAL sha256.
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

    // Write index.json
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

/// Build a modern `docker save` outer tar (OCI-layout export, Docker
/// 25+/containerd image store): layers at `blobs/sha256/<digest>` (no
/// `.tar` suffix) + a compat `manifest.json` whose `Layers` point at those
/// blob paths. `corrupt_digest` flips the layer path's digest so the blob's
/// real sha256 no longer matches (to exercise fail-closed verification).
pub(super) fn make_modern_docker_save(layer_tar: &[u8], corrupt_digest: bool) -> Vec<u8> {
    let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
    let config_hex = sha256_hex_of(&config);
    let layer_hex = if corrupt_digest {
        "0".repeat(64)
    } else {
        sha256_hex_of(layer_tar)
    };
    let manifest = serde_json::to_vec(&serde_json::json!([{
        "Config": format!("blobs/sha256/{config_hex}"),
        "RepoTags": ["modern:latest"],
        "Layers": [format!("blobs/sha256/{layer_hex}")],
    }]))
    .unwrap();

    let entries: Vec<(String, Vec<u8>)> = vec![
        (
            "oci-layout".to_string(),
            br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
        ),
        ("manifest.json".to_string(), manifest),
        (format!("blobs/sha256/{config_hex}"), config),
        (format!("blobs/sha256/{layer_hex}"), layer_tar.to_vec()),
    ];
    let mut outer = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut outer);
        for (path, data) in &entries {
            let mut h = tar::Header::new_gnu();
            h.set_path(path).unwrap();
            h.set_mode(0o644);
            h.set_size(data.len() as u64);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            tar.append(&h, data.as_slice()).unwrap();
        }
        tar.finish().unwrap();
    }
    outer
}
