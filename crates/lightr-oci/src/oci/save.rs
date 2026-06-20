//! WP-IMG-04 — `oci save <ref> [-o out.tar]`: export an image to a tar.
//!
//! The inverse of `import` (import.rs). Emits an **OCI-layout-in-tar**
//! (skopeo/`docker save`-style) so a later `oci load`/`import` round-trips it.
//!
//! Two modes, fail-closed on an absent ref / unwritable path:
//!
//!   • **Faithful (from record).** If the ref has a retained
//!     [`ImageManifestRecord`] (WP-IMG-01), emit the ORIGINAL manifest +
//!     original config/layer blobs VERBATIM. The blob bytes are read back from
//!     the CAS and laid out at `blobs/sha256/<sha256-hex-of-the-bytes>`; because
//!     the bytes are byte-for-byte the originals, their sha256 equals the
//!     `sha256:` references already inside the verbatim manifest — so an
//!     `oci load` of this tar reproduces the pulled image byte-for-byte.
//!
//!   • **Synth fallback (no record).** A locally-built / `snapshot`'d ref has no
//!     record. Synthesize a minimal but valid single-layer OCI-layout tar from
//!     the CAS tree (the same honest synthesis `oci push` performs) and report
//!     `faithful = false` so the caller can say so honestly (lossy).
//!
//! Output is written to `-o <file>` or, when `None`, to stdout (the Docker
//! default). The tar is built in memory then flushed in one write — `docker
//! save` images are small enough, and this keeps the writer-selection (file vs
//! stdout) trivially fail-closed.

use super::model::SaveReport;
use super::push::build_layer_tar_gz;
use super::util::{host_arch, sha256_hex_of, TempDirGuard};
use lightr_core::{LightrError, Manifest, Result};
use lightr_store::{ImageManifestRecord, Store};
use std::{fs, io::Write, path::Path};

/// Media type of the OCI image config descriptor (used to tell the config blob
/// apart from layer blobs in a retained record's descriptor list).
const OCI_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";

/// Export the image stored as `name` to an OCI-layout tar, written to `output`
/// (a path) or stdout (`None`). Fail-closed: an absent ref is `RefNotFound`
/// (exit 2); an unwritable path is `Io` (exit 1).
///
/// Returns a [`SaveReport`] recording the destination, layer count, byte size,
/// and whether the export was faithful (from a retained record) or synthesized
/// (lossy fallback from the CAS tree).
pub fn save(name: &str, output: Option<&Path>, store: &Store) -> Result<SaveReport> {
    // Resolve the ref first — fail-closed if absent (never an empty tar).
    let rec = store
        .ref_get(name)?
        .ok_or_else(|| LightrError::RefNotFound(name.to_string()))?;

    let (tar_bytes, layers, faithful) = match store.image_manifest_get(name)? {
        Some(record) => build_faithful_tar(store, &record)?,
        None => build_synth_tar(store, &rec.root)?,
    };

    let size = tar_bytes.len() as u64;
    write_out(&tar_bytes, output)?;

    let destination = match output {
        Some(p) => p.display().to_string(),
        None => "-".to_string(),
    };
    Ok(SaveReport {
        destination,
        layers,
        size,
        faithful,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Faithful export — verbatim manifest + verbatim config/layer blobs
// ─────────────────────────────────────────────────────────────────────────────

/// Build a faithful OCI-layout tar from a retained [`ImageManifestRecord`].
///
/// Emits: `oci-layout`, every config/layer blob at `blobs/sha256/<sha256-hex>`
/// (verbatim bytes, path keyed by the sha256 OF those bytes), the verbatim
/// manifest at `blobs/sha256/<manifest-sha256>`, a docker-`save`-compat
/// `manifest.json` (Config + Layers point at the blob paths so `import`'s
/// docker-save reader resolves them), and an `index.json` pointing at the
/// manifest blob. Returns `(tar_bytes, layer_count, faithful = true)`.
fn build_faithful_tar(store: &Store, record: &ImageManifestRecord) -> Result<(Vec<u8>, u64, bool)> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(record.descriptors.len() + 4);
    entries.push((
        "oci-layout".to_string(),
        br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
    ));

    // Split descriptors into the single config (by media type) and the layers,
    // preserving layer order. Read each blob back from the CAS verbatim and lay
    // it out at blobs/sha256/<sha256-of-bytes>.
    let mut config_path: Option<String> = None;
    let mut layer_paths: Vec<String> = Vec::new();
    let mut layers: u64 = 0;
    for desc in &record.descriptors {
        let bytes = store.get_bytes(&desc.digest)?;
        let hex = sha256_hex_of(&bytes);
        let blob_path = format!("blobs/sha256/{hex}");
        if desc.media_type == OCI_CONFIG_MEDIA_TYPE && config_path.is_none() {
            config_path = Some(blob_path.clone());
        } else {
            layer_paths.push(blob_path.clone());
            layers += 1;
        }
        entries.push((blob_path, bytes));
    }

    // The verbatim original manifest as its own blob (sha256-addressed).
    let manifest_hex = sha256_hex_of(&record.manifest_bytes);
    entries.push((
        format!("blobs/sha256/{manifest_hex}"),
        record.manifest_bytes.clone(),
    ));

    // docker-save-compat manifest.json — the path `import` reads first. Config
    // is "" when no config blob was retained (best-effort at import); `import`'s
    // reader skips an empty Config gracefully.
    let manifest_json = serde_json::to_vec(&serde_json::json!([{
        "Config": config_path.unwrap_or_default(),
        "Layers": layer_paths,
    }]))
    .map_err(|e| LightrError::InvalidManifest(format!("save manifest.json serialize: {e}")))?;
    entries.push(("manifest.json".to_string(), manifest_json));

    // index.json pointing at the verbatim manifest blob (OCI-layout faithful).
    let index = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{manifest_hex}"),
            "size": record.manifest_bytes.len(),
        }],
    }))
    .map_err(|e| LightrError::InvalidManifest(format!("save index.json serialize: {e}")))?;
    entries.push(("index.json".to_string(), index));

    let tar = pack_tar(&entries)?;
    Ok((tar, layers, true))
}

// ─────────────────────────────────────────────────────────────────────────────
// Synth fallback — single synthesized layer from the CAS tree (lossy)
// ─────────────────────────────────────────────────────────────────────────────

/// Build a minimal but valid single-layer OCI-layout tar from the CAS tree at
/// `root` (no retained record). The layer is synthesized exactly as `oci push`
/// does (one gzipped tar of the tree). Returns `(tar_bytes, 1, faithful=false)`.
fn build_synth_tar(store: &Store, root: &lightr_core::Digest) -> Result<(Vec<u8>, u64, bool)> {
    let manifest_bytes = store.get_bytes(root)?;
    let tree = Manifest::decode(&manifest_bytes)?;

    // Synthesize the single gzipped layer to a tempfile (RAM-bounded), exactly
    // like push; read it back to embed verbatim in the export tar.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp_dir = std::env::temp_dir().join(format!("lightr-oci-save-{pid}-{nanos}"));
    fs::create_dir_all(&tmp_dir).map_err(LightrError::Io)?;
    let _guard = TempDirGuard(tmp_dir.clone());
    let layer_path = tmp_dir.join("layer.tar.gz");

    let (layer_hex, diff_id_hex, layer_size) = build_layer_tar_gz(&tree, store, &layer_path)?;
    let layer_bytes = fs::read(&layer_path).map_err(LightrError::Io)?;

    // Minimal Linux config (single diff_id) — mirrors push's no-config branch.
    let config_bytes = serde_json::to_vec(&serde_json::json!({
        "architecture": host_arch(),
        "os": "linux",
        "rootfs": { "type": "layers", "diff_ids": [format!("sha256:{diff_id_hex}")] },
        "config": {},
    }))
    .map_err(|e| LightrError::InvalidManifest(format!("save config serialize: {e}")))?;
    let config_hex = sha256_hex_of(&config_bytes);

    // OCI image manifest referencing the synthesized config + single layer.
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": OCI_CONFIG_MEDIA_TYPE,
            "digest": format!("sha256:{config_hex}"),
            "size": config_bytes.len(),
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": format!("sha256:{layer_hex}"),
            "size": layer_size,
        }],
    }))
    .map_err(|e| LightrError::InvalidManifest(format!("save manifest serialize: {e}")))?;
    let manifest_hex = sha256_hex_of(&manifest);

    let manifest_json = serde_json::to_vec(&serde_json::json!([{
        "Config": format!("blobs/sha256/{config_hex}"),
        "Layers": [format!("blobs/sha256/{layer_hex}")],
    }]))
    .map_err(|e| LightrError::InvalidManifest(format!("save manifest.json serialize: {e}")))?;

    let index = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{manifest_hex}"),
            "size": manifest.len(),
        }],
    }))
    .map_err(|e| LightrError::InvalidManifest(format!("save index.json serialize: {e}")))?;

    let entries: Vec<(String, Vec<u8>)> = vec![
        (
            "oci-layout".to_string(),
            br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
        ),
        (format!("blobs/sha256/{config_hex}"), config_bytes),
        (format!("blobs/sha256/{layer_hex}"), layer_bytes),
        (format!("blobs/sha256/{manifest_hex}"), manifest),
        ("manifest.json".to_string(), manifest_json),
        ("index.json".to_string(), index),
    ];
    let tar = pack_tar(&entries)?;
    Ok((tar, 1, false))
}

// ─────────────────────────────────────────────────────────────────────────────
// tar packing + output sink
// ─────────────────────────────────────────────────────────────────────────────

/// Pack `(path, bytes)` entries into an uncompressed tar in memory. Each entry
/// is a regular 0o644 file (the OCI-layout-in-tar shape `import` reads).
fn pack_tar(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header
                .set_path(path)
                .map_err(|e| LightrError::InvalidManifest(format!("bad tar path {path}: {e}")))?;
            header.set_mode(0o644);
            header.set_size(data.len() as u64);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append(&header, data.as_slice())
                .map_err(LightrError::Io)?;
        }
        builder.finish().map_err(LightrError::Io)?;
    }
    Ok(out)
}

/// Write the tar to `output` (a path) or stdout (`None`). Fail-closed: an
/// unwritable path surfaces as `LightrError::Io` (exit 1).
fn write_out(bytes: &[u8], output: Option<&Path>) -> Result<()> {
    match output {
        Some(path) => fs::write(path, bytes).map_err(LightrError::Io),
        None => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            handle.write_all(bytes).map_err(LightrError::Io)?;
            handle.flush().map_err(LightrError::Io)
        }
    }
}

#[cfg(test)]
#[path = "tests/save_tests.rs"]
mod save_tests;
