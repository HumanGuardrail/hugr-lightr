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
}
