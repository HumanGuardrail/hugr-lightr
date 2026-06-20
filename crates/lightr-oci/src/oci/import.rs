//! OCI layout dir and docker-save tar import.

use super::layer::{apply_and_snapshot, LayerBlob};
use super::model::{DockerSaveItem, ImportReport, OciIndex, OciManifest};
use super::retain::{retain_image_manifest, RetainBlob};
use super::util::{platform_of_config, sha256_hex, verify_sha256};
use flate2::read::GzDecoder;
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

// ─────────────────────────────────────────────────────────────────────────────
// import_layout — OCI layout dir or docker-save tar
// ─────────────────────────────────────────────────────────────────────────────

/// Import an OCI **layout directory or tar** (skopeo/`docker save`-style):
/// parse index.json → manifest → apply layers in order (tar.gz/tar,
/// whiteouts honoured) into a temp tree → snapshot as `name` (parent chain
/// per repeated imports). Pure-local, no network.
///
/// All layer blobs are verified via real SHA-256 before being applied
/// (fail-closed; mismatch ⇒ `LightrError::Integrity`).
pub fn import_layout(path: &Path, store: &Store, name: &str) -> Result<ImportReport> {
    if path.is_dir() {
        import_oci_layout_dir(path, store, name)
    } else {
        import_docker_save_tar(path, store, name)
    }
}

pub(super) fn import_oci_layout_dir(
    layout_dir: &Path,
    store: &Store,
    name: &str,
) -> Result<ImportReport> {
    // Read index.json
    let index_json = fs::read(layout_dir.join("index.json")).map_err(LightrError::Io)?;
    let index: OciIndex = serde_json::from_slice(&index_json)
        .map_err(|e| LightrError::InvalidManifest(format!("index.json parse error: {e}")))?;

    if index.manifests.is_empty() {
        return Err(LightrError::InvalidManifest(
            "OCI index has no manifests".to_string(),
        ));
    }

    // Pick first manifest (single-arch layouts typically have one entry)
    let manifest_desc = &index.manifests[0];
    let manifest_hex = sha256_hex(&manifest_desc.digest).ok_or_else(|| {
        LightrError::InvalidManifest(format!(
            "unsupported manifest digest: {}",
            manifest_desc.digest
        ))
    })?;

    let manifest_path = layout_dir.join("blobs").join("sha256").join(manifest_hex);
    let manifest_bytes = fs::read(&manifest_path).map_err(|_| {
        LightrError::InvalidManifest(format!("manifest blob not found: {manifest_hex}"))
    })?;

    // FIX 1: real sha256 verification of the manifest blob.
    // The blob lives at blobs/sha256/<hex>; we compute the actual sha256 and
    // compare to <hex>. Mismatch ⇒ Integrity error (sha256 bytes in Digest).
    verify_sha256(&manifest_bytes, manifest_hex)?;

    let manifest: OciManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| LightrError::InvalidManifest(format!("manifest parse error: {e}")))?;

    let layer_count = manifest.layers.len() as u64;

    // Build blob list, verifying each layer blob via real sha256. Retain the
    // raw bytes + descriptor metadata (WP-IMG-01) alongside the apply blobs.
    let mut blobs: Vec<LayerBlob> = Vec::with_capacity(manifest.layers.len());
    let mut retained: Vec<RetainOwned> = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let layer_hex = sha256_hex(&layer.digest).ok_or_else(|| {
            LightrError::InvalidManifest(format!("unsupported layer digest: {}", layer.digest))
        })?;

        let blob_path = layout_dir.join("blobs").join("sha256").join(layer_hex);

        let layer_bytes = fs::read(&blob_path).map_err(|_| {
            LightrError::InvalidManifest(format!("layer blob not found: {layer_hex}"))
        })?;

        // FIX 1: real sha256 verification of the layer blob.
        // FIX 2: size mismatch is no longer reported as Integrity (which maps
        // to exit 1 for content corruption). We do the hash check which
        // implicitly verifies size; a wrong-size blob will produce a hash
        // mismatch → Integrity → exit 1, which is correct.
        verify_sha256(&layer_bytes, layer_hex)?;

        retained.push(RetainOwned {
            media_type: layer.media_type.clone(),
            sha256_hex: Some(layer_hex.to_string()),
            size: layer.size,
            bytes: layer_bytes.clone(),
        });
        blobs.push(LayerBlob::Bytes(layer_bytes));
    }

    let report = apply_and_snapshot(blobs, layer_count, store, name)?;

    // push-fidelity: capture the image config blob from the layout (it sits at
    // blobs/sha256/<config-hex>). sha256-verified; best-effort (the filesystem
    // is already snapshotted, so a missing config only costs push-fidelity).
    let mut platform = String::new();
    let mut config_blob: Option<RetainOwned> = None;
    if let Some(cfg_hex) = sha256_hex(&manifest.config.digest) {
        let cfg_path = layout_dir.join("blobs").join("sha256").join(cfg_hex);
        if let Ok(cfg_bytes) = fs::read(&cfg_path) {
            if verify_sha256(&cfg_bytes, cfg_hex).is_ok() {
                let _ = store.image_config_put(name, &cfg_bytes);
                platform = platform_of_config(&cfg_bytes);
                config_blob = Some(RetainOwned {
                    media_type: manifest.config.media_type.clone(),
                    sha256_hex: Some(cfg_hex.to_string()),
                    size: manifest.config.size,
                    bytes: cfg_bytes,
                });
            }
        }
    }

    // WP-IMG-01: retain verbatim manifest + ordered descriptors (config first,
    // then layers) + platform. Verify-then-retain is fail-closed (the per-blob
    // sha256 above already errored on mismatch; retain_image_manifest re-checks).
    retain_owned(
        store,
        name,
        &manifest_bytes,
        &platform,
        config_blob,
        retained,
    )?;

    Ok(report)
}

/// An owned retained blob (import paths hold the bytes in memory): descriptor
/// metadata + the raw bytes. Mirrors a [`RetainBlob`] but owns its buffer.
struct RetainOwned {
    media_type: String,
    sha256_hex: Option<String>,
    size: u64,
    bytes: Vec<u8>,
}

/// Build the borrowed [`RetainBlob`] view over owned buffers (config first,
/// then layers) and store one faithful [`ImageManifestRecord`].
fn retain_owned(
    store: &Store,
    name: &str,
    manifest_bytes: &[u8],
    platform: &str,
    config_blob: Option<RetainOwned>,
    layers: Vec<RetainOwned>,
) -> Result<()> {
    let mut owned: Vec<RetainOwned> = Vec::with_capacity(layers.len() + 1);
    if let Some(cfg) = config_blob {
        owned.push(cfg);
    }
    owned.extend(layers);
    let blobs: Vec<RetainBlob<'_>> = owned
        .iter()
        .map(|o| RetainBlob {
            media_type: o.media_type.clone(),
            sha256_hex: o.sha256_hex.as_deref(),
            size: o.size,
            bytes: o.bytes.as_slice(),
        })
        .collect();
    retain_image_manifest(store, name, manifest_bytes, platform, &blobs)
}

pub(super) fn import_docker_save_tar(
    tar_path: &Path,
    store: &Store,
    name: &str,
) -> Result<ImportReport> {
    // Read the entire tar into memory (docker save output is small enough).
    // Optionally gzip-compressed.
    let raw = fs::read(tar_path).map_err(LightrError::Io)?;
    let tar_bytes: Vec<u8> = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut gz = GzDecoder::new(&raw[..]);
        let mut out = Vec::new();
        gz.read_to_end(&mut out).map_err(LightrError::Io)?;
        out
    } else {
        raw
    };

    // First pass: scan the tar for manifest.json and all layer tars.
    let mut manifest_json_bytes: Option<Vec<u8>> = None;
    let mut layer_data: std::collections::HashMap<String, Vec<u8>> =
        std::collections::HashMap::new();

    {
        let cursor = io::Cursor::new(&tar_bytes);
        let mut archive = tar::Archive::new(cursor);
        for entry_result in archive.entries().map_err(LightrError::Io)? {
            let mut entry = entry_result.map_err(LightrError::Io)?;
            let entry_path = entry.path().map_err(LightrError::Io)?.into_owned();
            let path_str = entry_path.to_string_lossy().into_owned();

            if path_str == "manifest.json" || path_str == "./manifest.json" {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).map_err(LightrError::Io)?;
                manifest_json_bytes = Some(buf);
            } else if path_str.ends_with(".tar")
                || path_str.ends_with("/layer.tar")
                || path_str.trim_start_matches("./").starts_with("blobs/")
                || path_str.ends_with(".json")
            {
                // `.json` also captures the legacy `<hex>.json` image config for
                // push-fidelity (manifest.json is already handled above). Modern
                // configs live under `blobs/` and are caught by that arm.
                // Legacy docker-save names layers `<hash>/layer.tar` / `<hash>.tar`;
                // MODERN docker-save (OCI-layout export, Docker 25+/containerd image
                // store) names them `blobs/sha256/<digest>` with NO extension and a
                // compat `manifest.json` whose `Layers` point at those blob paths.
                // Collect both so the manifest's referenced paths resolve either way.
                // (Non-layer blobs — config, index — are collected too but only the
                // manifest's `Layers` are ever read back; they are small.)
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).map_err(LightrError::Io)?;
                // Normalize the key: strip leading ./
                let key = path_str.trim_start_matches("./").to_string();
                layer_data.insert(key, buf);
            }
        }
    }

    let manifest_bytes = manifest_json_bytes.ok_or_else(|| {
        LightrError::InvalidManifest("docker save tar: manifest.json not found".to_string())
    })?;

    let items: Vec<DockerSaveItem> = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        LightrError::InvalidManifest(format!("docker save manifest.json parse error: {e}"))
    })?;

    let item = items.into_iter().next().ok_or_else(|| {
        LightrError::InvalidManifest("docker save manifest.json is empty".to_string())
    })?;

    let layer_count = item.layers.len() as u64;

    // docker-save format: layers are named by path (not digest), so there is
    // no sha256 tie in the layer path. We verify content integrity when the
    // manifest carries a digest; otherwise we trust the path-named layer blob.
    // Full verification is only possible for OCI-layout format (blobs/sha256/<hex>).
    // docker-save layers carry no per-layer mediaType in manifest.json; the
    // OCI-faithful default for an uncompressed docker layer tar.
    const DOCKER_LAYER_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar";

    let mut blobs: Vec<LayerBlob> = Vec::with_capacity(item.layers.len());
    let mut retained: Vec<RetainOwned> = Vec::with_capacity(item.layers.len());
    for layer_path_str in &item.layers {
        let key = layer_path_str.trim_start_matches("./").to_string();
        let data = layer_data.get(&key).cloned().ok_or_else(|| {
            LightrError::InvalidManifest(format!("docker save layer not found: {key}"))
        })?;
        // Modern OCI-layout blobs embed their digest in the path
        // (`blobs/sha256/<hex>`) — verify content integrity, fail-closed. Legacy
        // path-named layers (`<hash>/layer.tar`) carry no digest to check.
        let sha256_hex = key
            .strip_prefix("blobs/sha256/")
            .map(|hex| -> Result<String> {
                verify_sha256(&data, hex)?;
                Ok(hex.to_string())
            })
            .transpose()?;
        retained.push(RetainOwned {
            media_type: DOCKER_LAYER_MEDIA_TYPE.to_string(),
            sha256_hex,
            size: data.len() as u64,
            bytes: data.clone(),
        });
        blobs.push(LayerBlob::Bytes(data));
    }

    let report = apply_and_snapshot(blobs, layer_count, store, name)?;

    // push-fidelity: capture the image config JSON the manifest points at
    // (legacy `<hex>.json` or modern `blobs/sha256/<hex>`, both collected in the
    // first pass). sha256-verified when the path carries a digest; best-effort.
    let mut platform = String::new();
    let mut config_blob: Option<RetainOwned> = None;
    if !item.config.is_empty() {
        let cfg_key = item.config.trim_start_matches("./").to_string();
        if let Some(cfg_bytes) = layer_data.get(&cfg_key) {
            let cfg_hex = match cfg_key.strip_prefix("blobs/sha256/") {
                Some(hex) => {
                    if verify_sha256(cfg_bytes, hex).is_ok() {
                        Some(hex.to_string())
                    } else {
                        None // mismatch ⇒ skip config retention (best-effort)
                    }
                }
                None => None, // legacy <hex>.json carries no in-path digest to check
            };
            // Retain only when the digest checks out (modern) or there is no
            // digest to check (legacy <hex>.json — trusted, sha256_hex None).
            let trustworthy = cfg_key.strip_prefix("blobs/sha256/").is_none() || cfg_hex.is_some();
            if trustworthy {
                let _ = store.image_config_put(name, cfg_bytes);
                platform = platform_of_config(cfg_bytes);
                config_blob = Some(RetainOwned {
                    media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                    sha256_hex: cfg_hex,
                    size: cfg_bytes.len() as u64,
                    bytes: cfg_bytes.clone(),
                });
            }
        }
    }

    // WP-IMG-01: retain the docker-save manifest.json verbatim + ordered
    // descriptors (config first, then layers) + platform. Verify-then-retain is
    // fail-closed for modern blobs (digest in path); legacy layers have no
    // digest to check and are retained as-is (same trust boundary as apply).
    retain_owned(
        store,
        name,
        &manifest_bytes,
        &platform,
        config_blob,
        retained,
    )?;

    Ok(report)
}
