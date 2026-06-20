//! Image-config sidecar — image_config_put / image_config_get.
//!
//! The imgmeta sidecar stores the 32-byte CAS digest of the original OCI image
//! config JSON captured at `oci pull`/`import`, keyed by ref name.
//! A later `oci push` reads it back to re-emit a runnable image
//! (entrypoint/cmd/env preserved) instead of a config-less single layer.

use super::cas::{atomic_write, get_bytes, put_bytes, shard_parts};
use lightr_core::{Digest, LightrError, Result};
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

const IMG_MANIFEST_CODEC_VERSION: u32 = 1;

fn encode_manifest_record(rec: &ImageManifestRecord) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&IMG_MANIFEST_CODEC_VERSION.to_le_bytes());
    out.extend_from_slice(&(rec.manifest_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&rec.manifest_bytes);
    out.extend_from_slice(&(rec.platform.len() as u32).to_le_bytes());
    out.extend_from_slice(rec.platform.as_bytes());
    out.extend_from_slice(&(rec.descriptors.len() as u32).to_le_bytes());
    for d in &rec.descriptors {
        out.extend_from_slice(&(d.media_type.len() as u32).to_le_bytes());
        out.extend_from_slice(d.media_type.as_bytes());
        out.extend_from_slice(&d.digest.0);
        out.extend_from_slice(&d.size.to_le_bytes());
    }
    out
}

/// A bounds-checked cursor reader for the length-prefixed codec.
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| {
                LightrError::InvalidManifest("image manifest record truncated".into())
            })?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_le_bytes(a))
    }
    fn digest(&mut self) -> Result<Digest> {
        let s = self.take(32)?;
        let mut a = [0u8; 32];
        a.copy_from_slice(s);
        Ok(Digest(a))
    }
    fn string(&mut self, n: usize) -> Result<String> {
        let s = self.take(n)?;
        String::from_utf8(s.to_vec())
            .map_err(|_| LightrError::InvalidManifest("non-UTF8 in image manifest record".into()))
    }
}

fn decode_manifest_record(bytes: &[u8]) -> Result<ImageManifestRecord> {
    let mut r = Reader::new(bytes);
    let version = r.u32()?;
    if version != IMG_MANIFEST_CODEC_VERSION {
        return Err(LightrError::InvalidManifest(format!(
            "unknown image manifest record version: {version}"
        )));
    }
    let mlen = r.u64()? as usize;
    let manifest_bytes = r.take(mlen)?.to_vec();
    let plen = r.u32()? as usize;
    let platform = r.string(plen)?;
    let n = r.u32()? as usize;
    let mut descriptors = Vec::with_capacity(n);
    for _ in 0..n {
        let mtlen = r.u32()? as usize;
        let media_type = r.string(mtlen)?;
        let digest = r.digest()?;
        let size = r.u64()?;
        descriptors.push(ImageDescriptor {
            media_type,
            digest,
            size,
        });
    }
    Ok(ImageManifestRecord {
        manifest_bytes,
        descriptors,
        platform,
    })
}

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

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::Store;
    use tempfile::TempDir;

    fn tmp_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        (dir, store)
    }

    // ── image_config sidecar (push-fidelity) ──────────────────────────────────

    #[test]
    fn image_config_roundtrip_and_absent_is_none() {
        let (_dir, store) = tmp_store();
        // A ref with no captured config ⇒ None (push then synthesizes minimal).
        assert!(store.image_config_get("noconfig").unwrap().is_none());
        // Put + get roundtrips the exact bytes (content-addressed in the CAS).
        let cfg = br#"{"architecture":"amd64","os":"linux","config":{"Cmd":["sh"]}}"#;
        store.image_config_put("img", cfg).unwrap();
        assert_eq!(
            store.image_config_get("img").unwrap().as_deref(),
            Some(&cfg[..])
        );
        // Last-write-wins: a second put replaces the sidecar.
        let cfg2 = br#"{"os":"linux"}"#;
        store.image_config_put("img", cfg2).unwrap();
        assert_eq!(
            store.image_config_get("img").unwrap().as_deref(),
            Some(&cfg2[..])
        );
    }

    // ── R-IMGREC: image manifest record codec roundtrip ───────────────────────

    #[test]
    fn image_manifest_record_roundtrip_and_absent_is_none() {
        use crate::store::imgmeta::{ImageDescriptor, ImageManifestRecord};
        use lightr_core::Digest;

        let (_dir, store) = tmp_store();
        // Absent ⇒ None.
        assert!(store.image_manifest_get("nomani").unwrap().is_none());

        let rec = ImageManifestRecord {
            manifest_bytes: br#"{"schemaVersion":2,"layers":[]}"#.to_vec(),
            descriptors: vec![
                ImageDescriptor {
                    media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                    digest: Digest([1u8; 32]),
                    size: 123,
                },
                ImageDescriptor {
                    media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_string(),
                    digest: Digest([2u8; 32]),
                    size: 456789,
                },
            ],
            platform: "linux/amd64".to_string(),
        };
        store.image_manifest_put("mani", &rec).unwrap();
        let got = store.image_manifest_get("mani").unwrap().unwrap();
        assert_eq!(got, rec, "record must survive the length-prefixed codec");

        // Last-write-wins: a second put replaces the sidecar.
        let rec2 = ImageManifestRecord {
            manifest_bytes: b"{}".to_vec(),
            descriptors: vec![],
            platform: String::new(),
        };
        store.image_manifest_put("mani", &rec2).unwrap();
        assert_eq!(store.image_manifest_get("mani").unwrap().unwrap(), rec2);
    }
}
