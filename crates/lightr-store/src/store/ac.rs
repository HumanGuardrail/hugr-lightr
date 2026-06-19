//! Action Cache plane — ac_get / ac_put / list_ac.
//!
//! AC entries are keyed by a Digest, sharded 2/62, stored with atomic rename.

use std::fs;
use std::path::{Path, PathBuf};
use lightr_core::{Digest, Result};
use super::cas::{shard_parts, atomic_write};
use super::lock::write_guard;

// ── path helper ───────────────────────────────────────────────────────────────

/// AC path: <root>/ac/<2hex>/<62hex>
pub(super) fn ac_path(root: &Path, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("ac").join(pre).join(rest)
}

// ── AC methods (called from Store) ───────────────────────────────────────────

/// Read an AC entry.  Absent → Ok(None).
pub fn ac_get(root: &Path, key: &Digest) -> Result<Option<Vec<u8>>> {
    let path = ac_path(root, key);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    Ok(Some(bytes))
}

/// Write an AC entry atomically (overwrite via temp+rename).
pub fn ac_put(root: &Path, key: &Digest, value: &[u8]) -> Result<()> {
    let _wg = write_guard(root)?;
    let path = ac_path(root, key);
    let hex = key.to_hex();
    let (pre, _) = shard_parts(&hex);
    let shard = root.join("ac").join(pre);
    atomic_write(&shard, &path, value)?;
    Ok(())
}

/// Enumerate all raw AC values (decoded by caller). Order unspecified.
pub fn list_ac(root: &Path) -> Result<Vec<Vec<u8>>> {
    let ac_root = root.join("ac");
    if !ac_root.exists() {
        return Ok(vec![]);
    }

    let mut values: Vec<Vec<u8>> = Vec::new();

    let shards = match fs::read_dir(&ac_root) {
        Ok(d) => d,
        Err(_) => return Ok(vec![]),
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
            // Skip temp files from in-flight atomic writes.
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
            match fs::read(&file_path) {
                Ok(bytes) => values.push(bytes),
                Err(_) => continue,
            }
        }
    }

    Ok(values)
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::Store;

    fn tmp_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        (dir, store)
    }

    // ── ac ───────────────────────────────────────────────────────────────────

    #[test]
    fn ac_roundtrip() {
        let (_dir, store) = tmp_store();
        let key = Digest::of_bytes(b"ac-key");
        let val = b"ac-value-bytes";

        store.ac_put(&key, val).unwrap();
        let got = store.ac_get(&key).unwrap();
        assert_eq!(got.as_deref(), Some(val.as_slice()));
    }

    #[test]
    fn ac_overwrite() {
        let (_dir, store) = tmp_store();
        let key = Digest::of_bytes(b"ac-key-2");

        store.ac_put(&key, b"first").unwrap();
        store.ac_put(&key, b"second").unwrap();
        let got = store.ac_get(&key).unwrap();
        assert_eq!(got.as_deref(), Some(b"second".as_slice()));
    }

    #[test]
    fn ac_absent_returns_none() {
        let (_dir, store) = tmp_store();
        let key = Digest::of_bytes(b"never-put");
        assert!(store.ac_get(&key).unwrap().is_none());
    }

    // ── R1: list_ac ──────────────────────────────────────────────────────────

    #[test]
    fn list_ac_roundtrip_two_values() {
        let (_dir, store) = tmp_store();

        let key1 = Digest::of_bytes(b"k1");
        let key2 = Digest::of_bytes(b"k2");
        let val1 = b"value-one";
        let val2 = b"value-two";

        store.ac_put(&key1, val1).unwrap();
        store.ac_put(&key2, val2).unwrap();

        let mut values = store.list_ac().unwrap();
        values.sort(); // order unspecified per spec; sort for determinism.
        assert_eq!(values.len(), 2, "expected exactly 2 AC values");
        assert!(values.contains(&val1.to_vec()), "missing val1");
        assert!(values.contains(&val2.to_vec()), "missing val2");
    }

    #[test]
    fn list_ac_empty_store() {
        let (_dir, store) = tmp_store();
        assert!(store.list_ac().unwrap().is_empty());
    }
}
