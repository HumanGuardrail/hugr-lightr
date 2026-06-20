//! WP-IMG-01 — layer-blob + manifest RETENTION at pull/import.
//!
//! Pull/import hydrate an image into the CAS tree but historically DISCARDED
//! the original raw layer blobs + manifest, so a later `oci push` could only
//! synthesize a single collapsed layer. This module RETAINS, at pull AND
//! import time, the original raw (compressed) layer blobs + the original
//! manifest JSON bytes + the ordered descriptors (mediaType/digest/size) + the
//! platform — stored as one [`ImageManifestRecord`] via `Store::image_manifest_put`
//! — so the future faithful push (WP-IMG-02) can reproduce the pulled image
//! byte-for-byte.
//!
//! **Verify-then-retain (fail-closed):** a blob whose content does not match
//! its declared `sha256:<hex>` digest is an ERROR, never silently retained.
//! Blobs whose digest algorithm is not sha256 (no in-path/in-descriptor hash
//! to check) are retained as-is — the same trust boundary the apply path uses.
//!
//! The descriptor's `digest` field holds the **CAS digest** returned by
//! `put_bytes` (how `oci push` retrieves the raw bytes back); the original OCI
//! `sha256:` references live verbatim inside `manifest_bytes` (what the
//! registry expects re-emitted). Both are needed for a faithful push.

use super::util::verify_sha256;
use lightr_core::Result;
use lightr_store::{ImageDescriptor, ImageManifestRecord, Store};

/// One blob to retain: its OCI media type, the expected `sha256:<hex>` digest
/// string (verified before retention; `None` ⇒ no sha256 to check), its
/// declared size, and the raw (compressed) bytes.
pub(super) struct RetainBlob<'a> {
    pub(super) media_type: String,
    /// Expected `sha256:<hex>` digest, if the source declares one.
    pub(super) sha256_hex: Option<&'a str>,
    pub(super) size: u64,
    pub(super) bytes: &'a [u8],
}

/// Verify each blob (fail-closed), retain its raw bytes in the CAS, and store
/// one faithful [`ImageManifestRecord`] for `name` (config + layers in the
/// given order). Idempotent: a re-pull/-import of the same image overwrites the
/// record (last-write-wins) with identical content — CAS `put_bytes` dedups the
/// blobs, `image_manifest_put` replaces the sidecar.
///
/// `manifest_bytes` is the original manifest JSON verbatim; `platform` is e.g.
/// `"linux/amd64"` (empty ⇒ unspecified). `blobs` are retained in order.
pub(super) fn retain_image_manifest(
    store: &Store,
    name: &str,
    manifest_bytes: &[u8],
    platform: &str,
    blobs: &[RetainBlob<'_>],
) -> Result<()> {
    let mut descriptors = Vec::with_capacity(blobs.len());
    for b in blobs {
        // Verify-then-retain: a declared sha256 that does not match the bytes is
        // a hard error (LightrError::Integrity), never silently retained.
        if let Some(hex) = b.sha256_hex {
            verify_sha256(b.bytes, hex)?;
        }
        // Retain the raw bytes in the CAS; the returned CAS digest is how a
        // later faithful push reads the blob back. put_bytes is idempotent.
        let cas_digest = store.put_bytes(b.bytes)?;
        descriptors.push(ImageDescriptor {
            media_type: b.media_type.clone(),
            digest: cas_digest,
            size: b.size,
        });
    }

    let rec = ImageManifestRecord {
        manifest_bytes: manifest_bytes.to_vec(),
        descriptors,
        platform: platform.to_string(),
    };
    store.image_manifest_put(name, &rec)
}
