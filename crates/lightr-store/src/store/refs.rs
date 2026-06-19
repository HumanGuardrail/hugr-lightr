//! Refs plane — ref_get / ref_put / ref_log / list_refs.
//!
//! Refs are keyed by a hash of the ref name, sharded 2/62.
//! Each ref write also appends an immutable log entry and a name record (once).

use super::cas::{atomic_write, shard_parts};
use super::lock::write_guard;
use lightr_core::{Digest, RefRecord, Result};
use std::fs;
use std::path::{Path, PathBuf};

// ── path helpers ──────────────────────────────────────────────────────────────

/// Ref path: <root>/refs/<2hex>/<62hex of ref_key digest>
pub(super) fn ref_path(root: &Path, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("refs").join(pre).join(rest)
}

/// Refs-names path: <root>/refs-names/<2hex>/<62hex of ref_key digest>
/// Content = UTF-8 ref name bytes, written once.
fn refs_names_path(root: &Path, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("refs-names").join(pre).join(rest)
}

/// Refs-log directory: <root>/refs-log/<2hex>/<62hex of ref_key digest>/
/// Each file is named `<n>` (decimal) and contains an encoded RefRecord.
fn refs_log_dir(root: &Path, key: &Digest) -> PathBuf {
    let hex = key.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("refs-log").join(pre).join(rest)
}

// ── ref methods (called from Store) ─────────────────────────────────────────

/// Read a ref.  `name` is validated; absent → Ok(None).
pub fn ref_get(root: &Path, name: &str) -> Result<Option<RefRecord>> {
    lightr_core::validate_ref_name(name)?;
    let key = lightr_core::ref_key(name);
    let path = ref_path(root, &key);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    let rec = RefRecord::decode(&bytes)?;
    Ok(Some(rec))
}

/// Write a ref atomically (last-write-wins).
/// R1 extension: also writes a name record (once) and appends a log entry.
pub fn ref_put(root: &Path, rec: &RefRecord) -> Result<()> {
    let _wg = write_guard(root)?;
    lightr_core::validate_ref_name(&rec.name)?;
    let key = lightr_core::ref_key(&rec.name);

    // 1. Write name record if absent (written once; idempotent).
    let names_path = refs_names_path(root, &key);
    if !names_path.exists() {
        let names_hex = key.to_hex();
        let (names_pre, _) = shard_parts(&names_hex);
        let names_shard = root.join("refs-names").join(names_pre);
        atomic_write(&names_shard, &names_path, rec.name.as_bytes())?;
    }

    // 2. Determine next log index by counting existing entries in log dir.
    let log_dir = refs_log_dir(root, &key);
    let next_n: u64 = if log_dir.exists() {
        match fs::read_dir(&log_dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|s| s.parse::<u64>().is_ok())
                        .unwrap_or(false)
                })
                .count() as u64,
            Err(_) => 0,
        }
    } else {
        0
    };

    // 3. Atomic-write log entry <n>.
    let log_entry_path = log_dir.join(next_n.to_string());
    let data = rec.encode();
    atomic_write(&log_dir, &log_entry_path, &data)?;

    // 4. Atomic-write the current ref file (LWW).
    let path = ref_path(root, &key);
    let hex = key.to_hex();
    let (pre, _) = shard_parts(&hex);
    let shard = root.join("refs").join(pre);
    atomic_write(&shard, &path, &data)?;

    Ok(())
}

/// Ref history, newest-first (index 0 = current).
/// Absent or empty log ⇒ Ok(vec![]). Corrupt entries are skipped silently.
pub fn ref_log(root: &Path, name: &str) -> Result<Vec<RefRecord>> {
    lightr_core::validate_ref_name(name)?;
    let key = lightr_core::ref_key(name);
    let log_dir = refs_log_dir(root, &key);

    if !log_dir.exists() {
        return Ok(vec![]);
    }

    // Collect all numeric file names in the log dir.
    let mut indices: Vec<u64> = match fs::read_dir(&log_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse::<u64>().ok()))
            .collect(),
        Err(_) => return Ok(vec![]),
    };

    if indices.is_empty() {
        return Ok(vec![]);
    }

    // Sort descending: newest-first (highest index = most recent write).
    indices.sort_unstable_by(|a, b| b.cmp(a));

    let mut records = Vec::with_capacity(indices.len());
    for n in indices {
        let entry_path = log_dir.join(n.to_string());
        match fs::read(&entry_path) {
            Ok(bytes) => match RefRecord::decode(&bytes) {
                Ok(rec) => records.push(rec),
                Err(_) => {
                    // Corrupt entry — skip silently (log is history, not truth).
                }
            },
            Err(_) => {
                // Missing or unreadable — skip silently.
            }
        }
    }

    Ok(records)
}

/// Enumerate all ref names ever written (from refs-names shards).
/// Non-UTF-8 name files are skipped. Returns sorted ascending.
pub fn list_refs(root: &Path) -> Result<Vec<String>> {
    let names_root = root.join("refs-names");
    if !names_root.exists() {
        return Ok(vec![]);
    }

    let mut names: Vec<String> = Vec::new();

    let shards = match fs::read_dir(&names_root) {
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
            if !file_path.is_file() {
                continue;
            }
            match fs::read(&file_path) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(name) => names.push(name),
                    Err(_) => {
                        // Non-UTF-8 name — skip per spec.
                    }
                },
                Err(_) => continue,
            }
        }
    }

    names.sort_unstable();
    Ok(names)
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use lightr_core::LightrError;
    use tempfile::TempDir;

    fn tmp_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        (dir, store)
    }

    fn make_ref_record(name: &str) -> RefRecord {
        RefRecord {
            name: name.to_string(),
            root: Digest::of_bytes(name.as_bytes()),
            parent: None,
            created_at_unix: 1_700_000_000,
            tool_version: "0.1.0".to_string(),
        }
    }

    // ── refs ─────────────────────────────────────────────────────────────────

    #[test]
    fn ref_roundtrip() {
        let (_dir, store) = tmp_store();
        let rec = make_ref_record("main");

        store.ref_put(&rec).unwrap();
        let got = store.ref_get("main").unwrap();
        assert!(got.is_some());
        let got = got.unwrap();
        assert_eq!(got.name, rec.name);
        assert_eq!(got.root, rec.root);
        assert_eq!(got.created_at_unix, rec.created_at_unix);
    }

    #[test]
    fn ref_last_write_wins() {
        let (_dir, store) = tmp_store();
        let rec1 = make_ref_record("dev");
        let mut rec2 = make_ref_record("dev");
        rec2.root = Digest::of_bytes(b"second root");

        store.ref_put(&rec1).unwrap();
        store.ref_put(&rec2).unwrap();

        let got = store.ref_get("dev").unwrap().unwrap();
        assert_eq!(got.root, rec2.root, "last-write-wins violated");
    }

    #[test]
    fn ref_absent_returns_none() {
        let (_dir, store) = tmp_store();
        let got = store.ref_get("nonexistent").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn ref_invalid_name_rejected() {
        let (_dir, store) = tmp_store();
        let rec = RefRecord {
            name: "INVALID NAME WITH SPACES".to_string(),
            root: Digest::of_bytes(b"x"),
            parent: None,
            created_at_unix: 0,
            tool_version: "0.1.0".to_string(),
        };
        let put_err = store.ref_put(&rec).unwrap_err();
        assert!(matches!(put_err, LightrError::InvalidRef(_)));
        let get_err = store.ref_get("INVALID NAME WITH SPACES").unwrap_err();
        assert!(matches!(get_err, LightrError::InvalidRef(_)));
    }

    // ── R1: ref_log ──────────────────────────────────────────────────────────

    #[test]
    fn ref_log_three_versions_newest_first() {
        let (_dir, store) = tmp_store();

        let root1 = Digest::of_bytes(b"v1");
        let root2 = Digest::of_bytes(b"v2");
        let root3 = Digest::of_bytes(b"v3");

        let rec1 = RefRecord {
            name: "main".to_string(),
            root: root1,
            parent: None,
            created_at_unix: 1_000,
            tool_version: "0.1.0".to_string(),
        };
        let rec2 = RefRecord {
            name: "main".to_string(),
            root: root2,
            parent: Some(root1),
            created_at_unix: 2_000,
            tool_version: "0.1.0".to_string(),
        };
        let rec3 = RefRecord {
            name: "main".to_string(),
            root: root3,
            parent: Some(root2),
            created_at_unix: 3_000,
            tool_version: "0.1.0".to_string(),
        };

        store.ref_put(&rec1).unwrap();
        store.ref_put(&rec2).unwrap();
        store.ref_put(&rec3).unwrap();

        let log = store.ref_log("main").unwrap();
        assert_eq!(log.len(), 3, "expected 3 log entries");
        // Index 0 = newest (rec3), 1 = rec2, 2 = oldest (rec1).
        assert_eq!(log[0].root, root3, "log[0] must be newest (v3)");
        assert_eq!(log[1].root, root2, "log[1] must be v2");
        assert_eq!(log[2].root, root1, "log[2] must be oldest (v1)");
    }

    #[test]
    fn ref_log_unknown_name_is_empty() {
        let (_dir, store) = tmp_store();
        let log = store.ref_log("does-not-exist").unwrap();
        assert!(log.is_empty(), "unknown ref must return empty log");
    }

    // R0 LWW still works after R1 extension.
    #[test]
    fn ref_log_lww_still_works() {
        let (_dir, store) = tmp_store();

        let root1 = Digest::of_bytes(b"first");
        let root2 = Digest::of_bytes(b"second");

        let rec1 = RefRecord {
            name: "dev".to_string(),
            root: root1,
            parent: None,
            created_at_unix: 100,
            tool_version: "0.1.0".to_string(),
        };
        let rec2 = RefRecord {
            name: "dev".to_string(),
            root: root2,
            parent: Some(root1),
            created_at_unix: 200,
            tool_version: "0.1.0".to_string(),
        };

        store.ref_put(&rec1).unwrap();
        store.ref_put(&rec2).unwrap();

        // ref_get must return the LWW (latest).
        let current = store.ref_get("dev").unwrap().unwrap();
        assert_eq!(current.root, root2, "LWW violated after R1 extension");
    }

    // ── R1: list_refs ─────────────────────────────────────────────────────────

    #[test]
    fn list_refs_returns_both_names_sorted() {
        let (_dir, store) = tmp_store();

        let rec_b = RefRecord {
            name: "beta".to_string(),
            root: Digest::of_bytes(b"beta"),
            parent: None,
            created_at_unix: 1,
            tool_version: "0.1.0".to_string(),
        };
        let rec_a = RefRecord {
            name: "alpha".to_string(),
            root: Digest::of_bytes(b"alpha"),
            parent: None,
            created_at_unix: 2,
            tool_version: "0.1.0".to_string(),
        };

        store.ref_put(&rec_b).unwrap();
        store.ref_put(&rec_a).unwrap();

        let refs = store.list_refs().unwrap();
        assert_eq!(
            refs,
            vec!["alpha", "beta"],
            "list_refs must be sorted ascending"
        );
    }

    #[test]
    fn list_refs_empty_store() {
        let (_dir, store) = tmp_store();
        assert!(store.list_refs().unwrap().is_empty());
    }
}
