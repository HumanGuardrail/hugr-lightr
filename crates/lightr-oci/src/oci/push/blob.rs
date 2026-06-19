//! Blob assembly and upload helpers for OCI push.
//!
//! Owns:
//! - `build_layer_tar_gz` — nested-tee gzip+hash streaming pipeline.
//! - `blob_exists` / `begin_blob_upload` / `upload_put_url` — per-request
//!   HTTP primitives.
//! - `upload_blob_from_bytes` / `upload_blob_from_file` — HEAD-skip +
//!   POST→PUT orchestration.

use super::super::http::retry_request;
use super::super::util::hasher_to_hex;
use flate2::{write::GzEncoder, Compression};
use lightr_core::{Entry, LightrError, Manifest, Result};
use lightr_store::Store;
use sha2::{Digest as Sha2Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

/// Assemble the tree into a gzipped tar at `dest`, streaming so neither the
/// uncompressed nor the compressed layer is fully buffered in RAM. Computes the
/// sha256 of the uncompressed tar (the OCI `diff_id`) AND of the gzipped tar
/// (the layer digest) on the fly.
///
/// Returns `(layer_digest_hex, diff_id_hex, gzipped_size_bytes)`.
///
/// `File` bytes are read from the CAS via `store.get_bytes`; `Symlink` and `Dir`
/// entries are emitted as the corresponding tar entry types.
pub(crate) fn build_layer_tar_gz(
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
pub(crate) fn blob_exists(
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
pub(crate) fn begin_blob_upload(
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
pub(crate) fn upload_put_url(
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
pub(crate) fn upload_blob_from_bytes(
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
pub(crate) fn upload_blob_from_file(
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

