//! OCI distribution v2 push implementation.

use super::http::{
    net_agent, read_creds_for_registry, registry_scheme, retry_request,
};
use super::model::PushReport;
use super::reference::{fetch_docker_token, parse_image_ref};
use super::util::{hasher_to_hex, host_arch, sha256_hex_of, TempDirGuard};
use flate2::{write::GzEncoder, Compression};
use lightr_core::{Entry, LightrError, Manifest, Result};
use lightr_store::Store;
use sha2::{Digest as Sha2Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

/// Push a stored ref to a registry as a **single-layer OCI image**.
///
/// # Imageless model — honest synthesis (NOT a blob round-trip)
///
/// Lightr's store keeps a content-addressed filesystem **tree** (a BLAKE3
/// `Manifest` of `File`/`Symlink`/`Dir` entries + their chunk objects), NOT the
/// original OCI layer blobs an image was imported from. There is therefore
/// nothing to "push back" verbatim. Instead `push` *synthesizes* a fresh,
/// spec-valid OCI image from the hydrated tree:
///
///   1. Resolve `name` → its `Manifest` (fail-closed if the ref is absent).
///   2. Assemble ONE tar layer from the tree (file bytes + mode, symlinks,
///      dirs), gzip it, streamed to a tempfile so RAM stays bounded.
///        - layer digest  = sha256 of the **gzipped** tar
///        - diff_id       = sha256 of the **uncompressed** tar
///   3. Build a minimal OCI image **config** (`rootfs.diff_ids = [diff_id]`).
///   4. Build the OCI image **manifest** (config descriptor + the one layer).
///   5. Upload config blob, layer blob (HEAD-skip if present, else
///      POST→PUT monolithic), then PUT the manifest under `<tag>`.
///
/// This is deliberately on-brand: the image is a faithful snapshot of the tree
/// the store actually holds, re-expressed in OCI's wire format. It will NOT be
/// byte-identical to whatever image the tree was first imported from (different
/// layer boundaries, no original history/config) — by design.
///
/// Network — bridge-only (ADR-0011). Auth reuses the pull machinery: a
/// PUSH-scoped bearer token for docker.io, Basic-from-config for other
/// registries (whose config creds already carry write perms).
pub fn push(name: &str, target: &str, store: &Store) -> Result<PushReport> {
    // a. Resolve the local ref → Manifest (fail-closed if missing).
    let rec = store
        .ref_get(name)?
        .ok_or_else(|| LightrError::RefNotFound(name.to_string()))?;
    let manifest_bytes = store.get_bytes(&rec.root)?;
    let tree = Manifest::decode(&manifest_bytes)?;

    // Parse/validate the target ref (empty/bad → InvalidRef → exit 2).
    let (registry, repo, tag) = parse_image_ref(target)?;
    let scheme = registry_scheme(&registry);
    let agent = net_agent();

    // b. Build the single tar layer streamed to a tempfile, computing BOTH the
    //    uncompressed (diff_id) and gzipped (layer digest) sha256 as we go.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp_dir = std::env::temp_dir().join(format!("lightr-oci-push-{pid}-{nanos}"));
    fs::create_dir_all(&tmp_dir).map_err(LightrError::Io)?;
    let _guard = TempDirGuard(tmp_dir.clone());
    let layer_path = tmp_dir.join("layer.tar.gz");

    let (layer_digest_hex, diff_id_hex, layer_size) =
        build_layer_tar_gz(&tree, store, &layer_path)?;

    // c. Build the OCI image config JSON.
    // push-fidelity: if the ORIGINAL config was captured at pull/import, re-emit
    // it — preserving entrypoint/cmd/env/workingdir/os/arch so the pushed image
    // RUNS identically — with ONLY `rootfs.diff_ids` replaced by the single
    // synthesized layer's diff_id (the original diff_ids described the original
    // layers, which we collapsed into one). Otherwise (a `snapshot`'d ref, or a
    // ref pulled before push-fidelity) synthesize a minimal Linux config.
    let config_bytes = match store.image_config_get(name)? {
        Some(orig) => {
            let mut v: serde_json::Value = serde_json::from_slice(&orig).map_err(|e| {
                LightrError::InvalidManifest(format!("stored image config parse error: {e}"))
            })?;
            v["rootfs"] = serde_json::json!({
                "type": "layers",
                "diff_ids": [format!("sha256:{diff_id_hex}")],
            });
            // `history` enumerates the ORIGINAL layers; with a single synthesized
            // layer it would disagree with diff_ids (some runtimes reject that),
            // so drop it. os/architecture/config (entrypoint/cmd/env) are kept.
            if let Some(obj) = v.as_object_mut() {
                obj.remove("history");
            }
            serde_json::to_vec(&v)
                .map_err(|e| LightrError::InvalidManifest(format!("config serialize error: {e}")))?
        }
        None => {
            // Minimal config. `os` MUST describe the IMAGE (Linux rootfs), not the
            // host that synthesized it — `std::env::consts::OS` would wrongly stamp
            // "macos" and make `docker run` warn. `architecture` = host arch.
            let config_json = serde_json::json!({
                "architecture": host_arch(),
                "os": "linux",
                "rootfs": {
                    "type": "layers",
                    "diff_ids": [format!("sha256:{diff_id_hex}")],
                },
                "config": {}
            });
            serde_json::to_vec(&config_json)
                .map_err(|e| LightrError::InvalidManifest(format!("config serialize error: {e}")))?
        }
    };
    let config_digest_hex = sha256_hex_of(&config_bytes);
    let config_size = config_bytes.len() as u64;

    // d. Build the OCI image manifest JSON.
    let image_manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": format!("sha256:{config_digest_hex}"),
            "size": config_size
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": format!("sha256:{layer_digest_hex}"),
            "size": layer_size
        }]
    });
    let image_manifest_bytes = serde_json::to_vec(&image_manifest)
        .map_err(|e| LightrError::InvalidManifest(format!("manifest serialize error: {e}")))?;
    let manifest_digest_hex = sha256_hex_of(&image_manifest_bytes);

    // e. Auth: PUSH scope for docker.io; Basic-from-config elsewhere.
    let creds = read_creds_for_registry(&registry);
    let auth_header: Option<String> = if registry == "registry-1.docker.io" {
        let token = fetch_docker_token(&agent, &repo, creds.as_ref(), "push,pull")?;
        Some(format!("Bearer {token}"))
    } else {
        creds.as_ref().map(|c| format!("Basic {}", c.b64))
    };
    let auth_ref = auth_header.as_deref();

    let repo_ref = format!("{registry}/{repo}");

    // g. Upload config blob, then layer blob (HEAD-skip → POST → monolithic PUT).
    upload_blob_from_bytes(
        &agent,
        scheme,
        &registry,
        &repo,
        auth_ref,
        &config_digest_hex,
        &config_bytes,
        &repo_ref,
    )?;
    upload_blob_from_file(
        &agent,
        scheme,
        &registry,
        &repo,
        auth_ref,
        &layer_digest_hex,
        &layer_path,
        layer_size,
        &repo_ref,
    )?;

    // PUT the manifest under <tag>.
    let manifest_url = format!("{scheme}{registry}/v2/{repo}/manifests/{tag}");
    retry_request(
        || {
            let mut req = agent
                .put(&manifest_url)
                .set("Content-Type", "application/vnd.oci.image.manifest.v1+json");
            if let Some(h) = auth_ref {
                req = req.set("Authorization", h);
            }
            req.send_bytes(&image_manifest_bytes)
        },
        &repo_ref,
    )?;

    // h. Return the report.
    Ok(PushReport {
        target: format!("{registry}/{repo}:{tag}"),
        manifest_digest: format!("sha256:{manifest_digest_hex}"),
        layers: 1,
        size: layer_size,
    })
}

/// Assemble the tree into a gzipped tar at `dest`, streaming so neither the
/// uncompressed nor the compressed layer is fully buffered in RAM. Computes the
/// sha256 of the uncompressed tar (the OCI `diff_id`) AND of the gzipped tar
/// (the layer digest) on the fly.
///
/// Returns `(layer_digest_hex, diff_id_hex, gzipped_size_bytes)`.
///
/// `File` bytes are read from the CAS via `store.get_bytes`; `Symlink` and `Dir`
/// entries are emitted as the corresponding tar entry types.
pub(super) fn build_layer_tar_gz(
    tree: &Manifest,
    store: &Store,
    dest: &Path,
) -> Result<(String, String, u64)> {
    /// A `Write` that tees bytes into a sha256 hasher AND an inner writer,
    /// counting the total written. Used twice: once around the gzip output
    /// (→ layer digest + size) and once between the tar and the gzip encoder
    /// (→ diff_id over the uncompressed tar).
    struct HashingWriter<W: Write> {
        inner: W,
        hasher: Sha256,
        count: u64,
    }
    impl<W: Write> Write for HashingWriter<W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let n = self.inner.write(buf)?;
            self.hasher.update(&buf[..n]);
            self.count += n as u64;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    let file = fs::File::create(dest).map_err(LightrError::Io)?;
    // Outer tee: hashes the GZIPPED bytes (layer digest) as they hit the file.
    let gz_hasher = HashingWriter {
        inner: io::BufWriter::new(file),
        hasher: Sha256::new(),
        count: 0,
    };
    let encoder = GzEncoder::new(gz_hasher, Compression::default());
    // Inner tee: hashes the UNCOMPRESSED tar bytes (diff_id) before gzip.
    let diff_hasher = HashingWriter {
        inner: encoder,
        hasher: Sha256::new(),
        count: 0,
    };
    let mut builder = tar::Builder::new(diff_hasher);

    for entry in &tree.entries {
        match entry {
            Entry::Dir { path } => {
                let mut header = tar::Header::new_gnu();
                header
                    .set_path(path)
                    .map_err(|e| LightrError::InvalidManifest(format!("bad dir path: {e}")))?;
                header.set_mode(0o755);
                header.set_size(0);
                header.set_entry_type(tar::EntryType::Directory);
                header.set_cksum();
                builder
                    .append(&header, io::empty())
                    .map_err(LightrError::Io)?;
            }
            Entry::Symlink { path, target } => {
                let mut header = tar::Header::new_gnu();
                header.set_size(0);
                header.set_mode(0o777);
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_link_name(target).map_err(|e| {
                    LightrError::InvalidManifest(format!("bad symlink target: {e}"))
                })?;
                builder
                    .append_data(&mut header, path, io::empty())
                    .map_err(LightrError::Io)?;
            }
            Entry::File {
                path,
                mode,
                size,
                digest,
            } => {
                let data = store.get_bytes(digest)?;
                let mut header = tar::Header::new_gnu();
                header.set_mode(*mode);
                header.set_size(*size);
                header.set_entry_type(tar::EntryType::Regular);
                builder
                    .append_data(&mut header, path, data.as_slice())
                    .map_err(LightrError::Io)?;
            }
        }
    }

    // Finish the tar → flush into gzip; recover the diff_id hasher/count.
    let diff_hasher = builder.into_inner().map_err(LightrError::Io)?;
    let diff_id_hex = hasher_to_hex(diff_hasher.hasher.clone());
    let encoder = diff_hasher.inner;
    // Finish gzip → recover the outer (gzipped) hasher + byte count.
    let gz_hasher = encoder.finish().map_err(LightrError::Io)?;
    let layer_size = gz_hasher.count;
    gz_hasher
        .inner
        .into_inner()
        .map_err(|e| LightrError::Io(io::Error::other(e.to_string())))?
        .sync_all()
        .map_err(LightrError::Io)?;
    let layer_digest_hex = hasher_to_hex(gz_hasher.hasher);

    Ok((layer_digest_hex, diff_id_hex, layer_size))
}

/// HEAD the blob; if present (200) return `true` (caller skips upload).
pub(super) fn blob_exists(
    agent: &ureq::Agent,
    scheme: &str,
    registry: &str,
    repo: &str,
    auth: Option<&str>,
    digest_hex: &str,
    repo_ref: &str,
) -> Result<bool> {
    let url = format!("{scheme}{registry}/v2/{repo}/blobs/sha256:{digest_hex}");
    // A 404 here is the normal "not present" answer — map it to Ok(false)
    // rather than letting it bubble up as a Registry error.
    let result = retry_request(
        || {
            let mut req = agent.head(&url);
            if let Some(h) = auth {
                req = req.set("Authorization", h);
            }
            req.call()
        },
        repo_ref,
    );
    match result {
        Ok(_) => Ok(true),
        Err(LightrError::Registry { status: 404, .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Start a monolithic blob upload: `POST /blobs/uploads/` → 202 + `Location`.
pub(super) fn begin_blob_upload(
    agent: &ureq::Agent,
    scheme: &str,
    registry: &str,
    repo: &str,
    auth: Option<&str>,
    repo_ref: &str,
) -> Result<String> {
    let url = format!("{scheme}{registry}/v2/{repo}/blobs/uploads/");
    let resp = retry_request(
        || {
            let mut req = agent.post(&url).set("Content-Length", "0");
            if let Some(h) = auth {
                req = req.set("Authorization", h);
            }
            req.call()
        },
        repo_ref,
    )?;
    resp.header("Location")
        .map(|s| s.to_string())
        .ok_or_else(|| LightrError::Registry {
            status: resp.status(),
            msg: "blob upload POST returned no Location header".to_string(),
        })
}

/// Append `digest=sha256:<hex>` to an upload `Location`, honoring an existing
/// query string (`?` vs `&`). The `Location` may be absolute or registry-relative.
pub(super) fn upload_put_url(
    scheme: &str,
    registry: &str,
    location: &str,
    digest_hex: &str,
) -> String {
    let base = if location.starts_with("http://") || location.starts_with("https://") {
        location.to_string()
    } else if let Some(rest) = location.strip_prefix('/') {
        format!("{scheme}{registry}/{rest}")
    } else {
        format!("{scheme}{registry}/{location}")
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}digest=sha256:{digest_hex}")
}

/// Upload an in-memory blob: HEAD-skip if present, else POST → monolithic PUT.
#[allow(clippy::too_many_arguments)]
pub(super) fn upload_blob_from_bytes(
    agent: &ureq::Agent,
    scheme: &str,
    registry: &str,
    repo: &str,
    auth: Option<&str>,
    digest_hex: &str,
    data: &[u8],
    repo_ref: &str,
) -> Result<()> {
    if blob_exists(agent, scheme, registry, repo, auth, digest_hex, repo_ref)? {
        return Ok(());
    }
    let location = begin_blob_upload(agent, scheme, registry, repo, auth, repo_ref)?;
    let put_url = upload_put_url(scheme, registry, &location, digest_hex);
    retry_request(
        || {
            let mut req = agent
                .put(&put_url)
                .set("Content-Type", "application/octet-stream");
            if let Some(h) = auth {
                req = req.set("Authorization", h);
            }
            req.send_bytes(data)
        },
        repo_ref,
    )?;
    Ok(())
}

/// Upload a blob streamed from a file: HEAD-skip if present, else POST →
/// monolithic PUT with the file as the request body (RAM-bounded).
#[allow(clippy::too_many_arguments)]
pub(super) fn upload_blob_from_file(
    agent: &ureq::Agent,
    scheme: &str,
    registry: &str,
    repo: &str,
    auth: Option<&str>,
    digest_hex: &str,
    path: &Path,
    size: u64,
    repo_ref: &str,
) -> Result<()> {
    if blob_exists(agent, scheme, registry, repo, auth, digest_hex, repo_ref)? {
        return Ok(());
    }
    let location = begin_blob_upload(agent, scheme, registry, repo, auth, repo_ref)?;
    let put_url = upload_put_url(scheme, registry, &location, digest_hex);
    retry_request(
        || {
            // Re-open the file per attempt so a retry restarts from byte 0.
            let file = fs::File::open(path)?;
            let mut req = agent
                .put(&put_url)
                .set("Content-Type", "application/octet-stream")
                .set("Content-Length", &size.to_string());
            if let Some(h) = auth {
                req = req.set("Authorization", h);
            }
            req.send(file)
        },
        repo_ref,
    )?;
    Ok(())
}
