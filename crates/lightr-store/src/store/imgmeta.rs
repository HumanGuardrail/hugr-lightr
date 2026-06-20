//! Image-config sidecar — image_config_put / image_config_get.
//!
//! The imgmeta sidecar stores the 32-byte CAS digest of the original OCI image
//! config JSON captured at `oci pull`/`import`, keyed by ref name.
//! A later `oci push` reads it back to re-emit a runnable image
//! (entrypoint/cmd/env preserved) instead of a config-less single layer.

use super::cas::{atomic_write, get_bytes, put_bytes, shard_parts};
use lightr_core::{Digest, Result};
use std::fs;
use std::path::{Path, PathBuf};

// ── R-IMGREC (parity-contract.md §0) — image manifest record + codec ────────
//
// `ImageManifestRecord` carries everything `oci push` needs to re-emit a
// FAITHFUL image: the original manifest bytes, the ordered layer/config
// descriptors, the platform, and the retained raw blob digests. The
// freeze-gate lands the record + a length-prefixed binary codec + put/get; the
// reachability behaviour (gc mark-walk marking the retained blobs reachable so
// gc never reaps faithful-push blobs) is **WP-IMG-01**'s job, NOT this gate.

/// One OCI descriptor as Lightr retains it: media type, the CAS digest of the
/// blob, and its size. `oci push` re-emits descriptors in the stored order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageDescriptor {
    pub media_type: String,
    pub digest: Digest,
    pub size: u64,
}

/// A faithful image manifest record (push-fidelity). Retained alongside the
/// raw blobs so a later `oci push` re-emits the EXACT manifest the registry
/// expects, not a synthesized single-layer image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageManifestRecord {
    /// The original manifest JSON bytes, verbatim.
    pub manifest_bytes: Vec<u8>,
    /// Ordered descriptors (config + layers) as they appear in the manifest.
    pub descriptors: Vec<ImageDescriptor>,
    /// Platform string, e.g. `"linux/amd64"`. Empty ⇒ unspecified.
    pub platform: String,
}

// ── length-prefixed binary codec ────────────────────────────────────────────
//
// Layout (all integers little-endian):
//   [u32 version=1]
//   [u64 manifest_len][manifest_bytes]
//   [u32 platform_len][platform_bytes]
//   [u32 n_descriptors]
//   repeat n: [u32 mt_len][mt_bytes][32 digest][u64 size]
// Self-describing + length-prefixed so a truncated/garbage record decodes to a
// clean InvalidManifest error rather than a panic.

#[path = "imgmeta_codec.rs"]
mod codec;
use codec::{decode_manifest_record, encode_manifest_record};

// ── path helper ───────────────────────────────────────────────────────────────

/// Image-config sidecar path: <root>/imgmeta/<2hex>/<62hex of ref_key digest>.
/// Content = the 32-byte CAS digest of the original OCI image config JSON
/// captured at `oci pull`/`import`, so `oci push` can re-emit a runnable image
/// (entrypoint/cmd/env preserved) instead of a config-less single layer.
pub(super) fn imgmeta_path(root: &Path, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("imgmeta").join(pre).join(rest)
}

// ── imgmeta methods (called from Store) ─────────────────────────────────────

/// Store the original OCI image config JSON for `name` (push-fidelity).
/// The config bytes are content-addressed in the CAS (dedup'd like any
/// object); the `imgmeta` sidecar records its digest keyed by the ref name,
/// last-write-wins. `put_bytes` takes its own (shared) write guard, so this
/// does not nest one. A later `oci push` reads it back via
/// `image_config_get` to re-emit a runnable image.
pub fn image_config_put(root: &Path, name: &str, config_bytes: &[u8]) -> Result<()> {
    lightr_core::validate_ref_name(name)?;
    let digest = put_bytes(root, config_bytes)?;
    let key = lightr_core::ref_key(name);
    let path = imgmeta_path(root, &key);
    let hex = key.to_hex();
    let (pre, _) = shard_parts(&hex);
    let shard = root.join("imgmeta").join(pre);
    atomic_write(&shard, &path, &digest.0)?;
    Ok(())
}

/// Read the original OCI image config JSON stored for `name`, if any.
/// `None` ⇒ no config was captured (a `snapshot`'d ref, or a ref pulled
/// before push-fidelity shipped) — `oci push` then synthesizes a minimal
/// config. A corrupt sidecar (not a 32-byte digest) is treated as absent
/// (fail-soft to the minimal config, never an error).
pub fn image_config_get(root: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    lightr_core::validate_ref_name(name)?;
    let key = lightr_core::ref_key(name);
    let path = imgmeta_path(root, &key);
    if !path.exists() {
        return Ok(None);
    }
    let dbytes = fs::read(&path)?;
    if dbytes.len() != 32 {
        return Ok(None);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&dbytes);
    let config = get_bytes(root, &Digest(arr))?;
    Ok(Some(config))
}

// ── WP-IMG-03: copy image sidecars (oci tag — ref alias fidelity) ────────────
//
// `oci tag <src> <dst>` aliases a ref to a new name with ZERO data copy: the
// ref pointer is repointed (in the refs plane) and the image sidecars are
// duplicated here so a later FAITHFUL `oci push` of `dst` reproduces the
// original image (entrypoint/cmd/env + exact manifest), not a synthesized
// single-layer image. Only the 32-byte sidecar POINTERS are copied — the CAS
// blobs they reference are shared (content-addressed, never duplicated). If
// `src` has no sidecar, copy is a no-op for that family (tag still works).

/// Copy the image sidecars (config + manifest) from `src` to `dst`, sharing the
/// underlying CAS blobs (no object duplication). Each family is copied only if
/// `src` has it; a corrupt `src` sidecar (not a 32-byte pointer) is skipped,
/// matching the fail-soft read path. Last-write-wins on `dst`.
pub fn copy_image_sidecars(root: &Path, src: &str, dst: &str) -> Result<()> {
    lightr_core::validate_ref_name(src)?;
    lightr_core::validate_ref_name(dst)?;

    copy_one_sidecar(root, "imgmeta", src, dst)?;
    copy_one_sidecar(root, "imgmanifest", src, dst)?;
    Ok(())
}

/// Copy a single sidecar family's 32-byte pointer from `src`'s key to `dst`'s
/// key under `<root>/<dir>/`. No-op if absent; skip (no-op) if the source
/// pointer is not exactly 32 bytes (corrupt — fail-soft, like the read path).
fn copy_one_sidecar(root: &Path, dir: &str, src: &str, dst: &str) -> Result<()> {
    let src_key = lightr_core::ref_key(src);
    let src_path = sidecar_path(root, dir, &src_key);
    if !src_path.exists() {
        return Ok(());
    }
    let pointer = fs::read(&src_path)?;
    if pointer.len() != 32 {
        return Ok(());
    }

    let dst_key = lightr_core::ref_key(dst);
    let dst_path = sidecar_path(root, dir, &dst_key);
    let hex = dst_key.to_hex();
    let (pre, _) = shard_parts(&hex);
    let shard = root.join(dir).join(pre);
    atomic_write(&shard, &dst_path, &pointer)?;
    Ok(())
}

// ── WP-IMG-07: remove image sidecars (oci rmi — untag) ───────────────────────
//
// `oci rmi <ref>` removes the ref's image sidecars (config + manifest record)
// so the image record disappears with the tag. Only the 32-byte sidecar
// POINTERS are deleted — the CAS blobs they referenced are left untouched
// (they become gc candidates, reclaimed by `lightr gc`, never swept here).

/// Remove both image sidecars (config + manifest) for `name`. Each family is
/// removed only if present (no-op otherwise — idempotent/fail-soft). Deletes
/// only the sidecar pointer file; the CAS blob it pointed at is NOT touched
/// (becomes a gc candidate). Used by `oci rmi`.
pub fn remove_image_sidecars(root: &Path, name: &str) -> Result<()> {
    lightr_core::validate_ref_name(name)?;
    remove_one_sidecar(root, "imgmeta", name)?;
    remove_one_sidecar(root, "imgmanifest", name)?;
    Ok(())
}

/// Remove a single sidecar family's pointer for `name` under `<root>/<dir>/`.
/// No-op if absent (idempotent). Only the pointer file is deleted.
fn remove_one_sidecar(root: &Path, dir: &str, name: &str) -> Result<()> {
    let key = lightr_core::ref_key(name);
    let path = sidecar_path(root, dir, &key);
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Sidecar pointer path for a family dir: `<root>/<dir>/<2hex>/<62hex of key>`.
fn sidecar_path(root: &Path, dir: &str, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join(dir).join(pre).join(rest)
}

// ── R-IMGREC: image MANIFEST record put/get ─────────────────────────────────
//
// Distinct sidecar dir (`imgmanifest/`) from the config sidecar (`imgmeta/`) so
// the two never collide. The length-prefixed record is content-addressed in the
// CAS like any blob; the sidecar records its digest keyed by ref name,
// last-write-wins — same shape as `image_config_put`.

/// Image-manifest sidecar path: `<root>/imgmanifest/<2hex>/<62hex of ref_key>`.
fn imgmanifest_path(root: &Path, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("imgmanifest").join(pre).join(rest)
}

/// Store the faithful [`ImageManifestRecord`] for `name` (push-fidelity).
/// Encoded with the length-prefixed codec, content-addressed in the CAS; the
/// sidecar records its digest keyed by ref name, last-write-wins. The gc
/// mark-walk extension that keeps the retained blobs reachable is WP-IMG-01.
pub fn image_manifest_put(root: &Path, name: &str, rec: &ImageManifestRecord) -> Result<()> {
    lightr_core::validate_ref_name(name)?;
    let encoded = encode_manifest_record(rec);
    let digest = put_bytes(root, &encoded)?;
    let key = lightr_core::ref_key(name);
    let path = imgmanifest_path(root, &key);
    let hex = key.to_hex();
    let (pre, _) = shard_parts(&hex);
    let shard = root.join("imgmanifest").join(pre);
    atomic_write(&shard, &path, &digest.0)?;
    Ok(())
}

/// Read the [`ImageManifestRecord`] stored for `name`, if any. `None` ⇒ no
/// record captured. A corrupt sidecar (not a 32-byte digest) is treated as
/// absent; a corrupt RECORD body surfaces as `InvalidManifest` (fail-closed, so
/// a faithful-push never silently emits a wrong manifest).
pub fn image_manifest_get(root: &Path, name: &str) -> Result<Option<ImageManifestRecord>> {
    lightr_core::validate_ref_name(name)?;
    let key = lightr_core::ref_key(name);
    let path = imgmanifest_path(root, &key);
    if !path.exists() {
        return Ok(None);
    }
    let dbytes = fs::read(&path)?;
    if dbytes.len() != 32 {
        return Ok(None);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&dbytes);
    let encoded = get_bytes(root, &Digest(arr))?;
    Ok(Some(decode_manifest_record(&encoded)?))
}

// ── WP-IMG-09 (R-IMGREC): gc reachability enumeration ───────────────────────
//
// IMG-01 retains the original config + layer blobs in the CAS, referenced ONLY
// by these `imgmanifest` (and `imgmeta` config) sidecars — surfaces the gc
// mark-walk does NOT otherwise traverse. Without this enumeration gc would reap
// the retained blobs and a later faithful `oci push` would lose layers.
//
// This accessor over-approximates by design (mark, never sweep retained — fail
// safe): it walks BOTH sidecar families and returns EVERY CAS digest they keep
// alive, so the caller (gc) can mark them reachable:
//   • the record blob itself (the encoded `ImageManifestRecord`, stored via
//     `put_bytes` and pointed at by the `imgmanifest` sidecar),
//   • each descriptor digest in the record (config + every layer blob),
//   • the config blob pointed at by each `imgmeta` config sidecar.
// `manifest_bytes` is carried INLINE in the record (not a separate CAS object),
// so it needs no digest of its own.
//
// Fail-soft per-entry (matching `list_ac`): a corrupt sidecar or a record that
// fails to decode is SKIPPED, never fatal — gc must not abort because one
// sidecar is garbage. Skipping at worst under-marks that single record (which
// is itself unreadable, hence unusable for push anyway); it never causes a live
// retained blob of a *valid* record to be reaped.

/// Walk one sidecar shard family (`<root>/<dir>/<2hex>/<62hex>`) and pass each
/// stored 32-byte CAS digest pointer to `f`. Skips `.tmp-` in-flight writes and
/// any file that is not exactly 32 bytes (corrupt sidecar ⇒ fail-soft).
fn for_each_sidecar_pointer(root: &Path, dir: &str, mut f: impl FnMut(Digest)) {
    let base = root.join(dir);
    let shards = match fs::read_dir(&base) {
        Ok(d) => d,
        Err(_) => return,
    };
    for shard_entry in shards.filter_map(|e| e.ok()) {
        let shard_path = shard_entry.path();
        if !shard_path.is_dir() {
            continue;
        }
        let files = match fs::read_dir(&shard_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for file_entry in files.filter_map(|e| e.ok()) {
            let file_path = file_entry.path();
            if file_path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.starts_with(".tmp-"))
                .unwrap_or(false)
            {
                continue;
            }
            if !file_path.is_file() {
                continue;
            }
            let dbytes = match fs::read(&file_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if dbytes.len() != 32 {
                continue;
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&dbytes);
            f(Digest(arr));
        }
    }
}

/// WP-IMG-09: enumerate EVERY CAS digest kept alive by the image sidecars, so
/// the gc mark-walk can mark them reachable (else gc reaps faithful-push blobs).
///
/// Returns: every `imgmanifest` record-blob digest + each of its descriptor
/// digests (config + layers) + every `imgmeta` config-blob digest. Order is
/// unspecified; duplicates are possible (the caller dedups via its mark set).
/// Fail-soft: corrupt/undecodable sidecars are skipped, never fatal.
pub fn list_image_reachable_blobs(root: &Path) -> Result<Vec<Digest>> {
    let mut out: Vec<Digest> = Vec::new();

    // imgmanifest sidecars: mark the record blob AND every descriptor blob.
    for_each_sidecar_pointer(root, "imgmanifest", |record_digest| {
        out.push(record_digest);
        // Decode the record to reach its config + layer descriptors. A blob we
        // cannot read/decode is skipped (fail-soft) — the record-blob pointer
        // itself is already marked above, so we never lose a readable record.
        if let Ok(encoded) = get_bytes(root, &record_digest) {
            if let Ok(rec) = decode_manifest_record(&encoded) {
                for d in &rec.descriptors {
                    out.push(d.digest);
                }
            }
        }
    });

    // imgmeta config sidecars: mark the original OCI config blob.
    for_each_sidecar_pointer(root, "imgmeta", |config_digest| {
        out.push(config_digest);
    });

    Ok(out)
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
#[path = "imgmeta_tests.rs"]
mod tests;
