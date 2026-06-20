//! OCI distribution v2 pull implementation.

use super::http::{
    net_agent, read_creds_for_registry, read_response_bytes, retry_request, stream_blob_to_file,
};
use super::layer::{apply_and_snapshot, LayerBlob};
use super::model::{ImportReport, ManifestList, OciManifest};
use super::reference::{fetch_docker_token, parse_image_ref, pick_from_manifest_list};
use super::retain::{retain_image_manifest, RetainBlob};
use super::util::sha256_hex;
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::fs;

/// Pull from a registry (OCI distribution v2; private auth + anonymous/bearer
/// token for docker.io), then import. Network — bridge-only.
///
/// Hardening (WP-A-pull):
///   - Private-registry auth via docker config.json / LIGHTR_REGISTRY_AUTH env.
///   - Retry + exponential backoff on 429 and 5xx.
///   - Streaming blob download (sha256 computed over the reader, never full Vec).
///   - Typed errors: 401/403 → Registry/auth, 404 → Registry/not-found, etc.
///   - Multi-arch: picks linux/<host>, falls back to amd64, then any linux.
pub fn pull(image: &str, store: &Store, name: &str) -> Result<ImportReport> {
    // Validate/parse image ref; reject empty/malformed refs → InvalidRef → exit 2.
    let (registry, repo, tag) = parse_image_ref(image)?;
    let agent = net_agent();

    // Resolve credentials for this registry.
    let creds = read_creds_for_registry(&registry);

    // Build the Authorization header value for requests to this registry.
    // For docker.io: if we have creds, use Basic on the token endpoint;
    // otherwise fall through to the anonymous bearer flow.
    let (bearer_token, basic_auth): (Option<String>, Option<String>) =
        if registry == "registry-1.docker.io" {
            // Docker Hub token endpoint — pass Basic creds if we have them,
            // or anonymous if not.
            let token = fetch_docker_token(&agent, &repo, creds.as_ref(), "pull")?;
            (Some(token), None)
        } else if let Some(ref c) = creds {
            // Other registries: use Basic auth directly.
            (None, Some(format!("Basic {}", c.b64)))
        } else {
            (None, None)
        };

    // Build the Authorization header string for per-request use.
    let auth_header: Option<String> = bearer_token
        .as_ref()
        .map(|t| format!("Bearer {t}"))
        .or_else(|| basic_auth.clone());

    let auth_ref: Option<&str> = auth_header.as_deref();

    // Fetch manifest (with retry).
    let manifest_url = format!("https://{registry}/v2/{repo}/manifests/{tag}");
    let resp = retry_request(
        || {
            let mut req = agent.get(&manifest_url).set(
                "Accept",
                "application/vnd.oci.image.manifest.v1+json, \
                     application/vnd.docker.distribution.manifest.v2+json, \
                     application/vnd.docker.distribution.manifest.list.v2+json, \
                     application/vnd.oci.image.index.v1+json",
            );
            if let Some(h) = auth_ref {
                req = req.set("Authorization", h);
            }
            req.call()
        },
        &format!("{registry}/{repo}:{tag}"),
    )?;

    let content_type = resp.content_type().to_string();
    let manifest_bytes = read_response_bytes(resp)?;

    // Handle manifest list / index — pick best linux arch. Capture the config
    // descriptor alongside the layers (push-fidelity: the config blob holds
    // entrypoint/cmd/env/os/arch, re-emitted by `oci push`).
    //
    // WP-IMG-01: capture the ACTUAL image manifest bytes (the resolved arch's
    // manifest when a list, else the top-level one) — that is the verbatim
    // manifest retained for a faithful push, NOT the index — plus the platform
    // from the chosen list descriptor.
    let mut platform = String::new();
    let (layer_descs, config_desc, image_manifest_bytes) = if content_type.contains("manifest.list")
        || content_type.contains("image.index")
    {
        let list: ManifestList = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| LightrError::InvalidManifest(format!("manifest list parse error: {e}")))?;

        let chosen = pick_from_manifest_list(&list.manifests)?;
        if let Some(p) = chosen.platform.as_ref() {
            platform = format!("{}/{}", p.os, p.architecture);
        }

        // Fetch the specific manifest (with retry).
        let spec_url = format!("https://{registry}/v2/{repo}/manifests/{}", chosen.digest);
        let resp2 = retry_request(
            || {
                let mut req2 = agent.get(&spec_url).set(
                    "Accept",
                    "application/vnd.oci.image.manifest.v1+json, \
                     application/vnd.docker.distribution.manifest.v2+json",
                );
                if let Some(h) = auth_ref {
                    req2 = req2.set("Authorization", h);
                }
                req2.call()
            },
            &format!("{registry}/{repo}"),
        )?;
        let bytes2 = read_response_bytes(resp2)?;
        let m: OciManifest = serde_json::from_slice(&bytes2)
            .map_err(|e| LightrError::InvalidManifest(format!("manifest parse error: {e}")))?;
        (m.layers, m.config, bytes2)
    } else {
        let m: OciManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| LightrError::InvalidManifest(format!("manifest parse error: {e}")))?;
        (m.layers, m.config, manifest_bytes.clone())
    };

    let layer_count = layer_descs.len() as u64;

    // Stream each layer blob to a temp file, computing sha256 streaming.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let blob_tmp_dir = std::env::temp_dir().join(format!("lightr-oci-pull-{pid}-{nanos}"));
    fs::create_dir_all(&blob_tmp_dir).map_err(LightrError::Io)?;
    let _blob_guard = super::util::TempDirGuard(blob_tmp_dir.clone());

    // Track each layer's temp-file path + descriptor metadata for WP-IMG-01
    // retention (the raw bytes are read back from these files after snapshot,
    // while the TempDirGuard is still alive).
    let mut blobs: Vec<LayerBlob> = Vec::with_capacity(layer_descs.len());
    let mut layer_files: Vec<RetainSource> = Vec::with_capacity(layer_descs.len());
    for (idx, layer) in layer_descs.iter().enumerate() {
        let blob_url = format!("https://{registry}/v2/{repo}/blobs/{}", layer.digest);

        let blob_file = if let Some(hex) = sha256_hex(&layer.digest) {
            // Named by sha256 hex for audit trail.
            let blob_file = blob_tmp_dir.join(hex);
            stream_blob_to_file(
                &agent,
                &blob_url,
                auth_ref,
                &blob_file,
                Some(hex),
                &format!("{registry}/{repo}"),
            )?;
            blob_file
        } else {
            // Non-sha256 digest algorithm: stream without hash check.
            let blob_file = blob_tmp_dir.join(format!("layer-{idx}.blob"));
            stream_blob_to_file(
                &agent,
                &blob_url,
                auth_ref,
                &blob_file,
                None,
                &format!("{registry}/{repo}"),
            )?;
            blob_file
        };
        layer_files.push(RetainSource {
            path: blob_file.clone(),
            media_type: layer.media_type.clone(),
            sha256_hex: sha256_hex(&layer.digest).map(str::to_string),
            size: layer.size,
        });
        blobs.push(LayerBlob::File(blob_file));
    }

    let report = apply_and_snapshot(blobs, layer_count, store, name)?;

    // push-fidelity: capture the original image config (entrypoint/cmd/env/os/arch)
    // so a later `oci push` re-emits a RUNNABLE image, not a config-less layer.
    // Best-effort: the image filesystem is already snapshotted above, so a
    // config-fetch hiccup must NOT fail the pull — push just falls back to a
    // synthesized minimal config. Verified by sha256 (Some(cfg_hex)).
    let mut config_file: Option<RetainSource> = None;
    if let Some(cfg_hex) = sha256_hex(&config_desc.digest) {
        let cfg_url = format!("https://{registry}/v2/{repo}/blobs/{}", config_desc.digest);
        let cfg_file = blob_tmp_dir.join(format!("config-{cfg_hex}"));
        let repo_disp = format!("{registry}/{repo}");
        if stream_blob_to_file(
            &agent,
            &cfg_url,
            auth_ref,
            &cfg_file,
            Some(cfg_hex),
            &repo_disp,
        )
        .is_ok()
        {
            if let Ok(cfg_bytes) = fs::read(&cfg_file) {
                let _ = store.image_config_put(name, &cfg_bytes);
            }
            config_file = Some(RetainSource {
                path: cfg_file,
                media_type: config_desc.media_type.clone(),
                sha256_hex: Some(cfg_hex.to_string()),
                size: config_desc.size,
            });
        }
    }

    // WP-IMG-01: retain the original (compressed) blobs + verbatim manifest +
    // ordered descriptors + platform, so WP-IMG-02's faithful push can reproduce
    // the image byte-for-byte. Verify-then-retain is fail-closed (a digest
    // mismatch errors). Best-effort: the image is already snapshotted, so a
    // retention I/O hiccup must NOT undo a successful pull — push then falls
    // back to a synthesized single layer. (A digest MISMATCH still surfaces.)
    retain_pulled(
        store,
        name,
        &image_manifest_bytes,
        &platform,
        &config_file,
        &layer_files,
    )?;

    Ok(report)
}

/// A retained blob still on disk under the pull TempDirGuard: its temp path +
/// the descriptor metadata needed to rebuild an [`ImageDescriptor`].
struct RetainSource {
    path: std::path::PathBuf,
    media_type: String,
    sha256_hex: Option<String>,
    size: u64,
}

/// Read each retained blob back from its temp file and store one faithful
/// [`ImageManifestRecord`] (config first, then layers in order). A digest
/// MISMATCH is fail-closed (propagated); a read I/O error is best-effort
/// (skipped — push falls back) since the image is already snapshotted.
fn retain_pulled(
    store: &Store,
    name: &str,
    manifest_bytes: &[u8],
    platform: &str,
    config_file: &Option<RetainSource>,
    layer_files: &[RetainSource],
) -> Result<()> {
    let mut buffers: Vec<(Vec<u8>, &RetainSource)> = Vec::new();
    if let Some(cfg) = config_file.as_ref() {
        match fs::read(&cfg.path) {
            Ok(b) => buffers.push((b, cfg)),
            Err(_) => return Ok(()), // best-effort: no faithful record this pull
        }
    }
    for src in layer_files {
        match fs::read(&src.path) {
            Ok(b) => buffers.push((b, src)),
            Err(_) => return Ok(()),
        }
    }
    let blobs: Vec<RetainBlob<'_>> = buffers
        .iter()
        .map(|(bytes, src)| RetainBlob {
            media_type: src.media_type.clone(),
            sha256_hex: src.sha256_hex.as_deref(),
            size: src.size,
            bytes: bytes.as_slice(),
        })
        .collect();
    retain_image_manifest(store, name, manifest_bytes, platform, &blobs)
}
