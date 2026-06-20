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

/// Remove a ref (untag) — WP-IMG-07. Deletes the current ref file AND the
/// name record so the ref vanishes from `ref_get` and `list_refs` (hence from
/// `oci images`). The immutable append-only log under `refs-log/` is preserved
/// (history, not truth — matches `ref_log`'s "log is history" contract). The
/// underlying CAS objects are NOT touched: they become gc candidates, reclaimed
/// by `lightr gc`, never swept here. Returns `Ok(false)` if the ref was already
/// absent (idempotent — fail-soft), `Ok(true)` if it existed and was removed.
pub fn ref_remove(root: &Path, name: &str) -> Result<bool> {
    let _wg = write_guard(root)?;
    lightr_core::validate_ref_name(name)?;
    let key = lightr_core::ref_key(name);

    let path = ref_path(root, &key);
    let names_path = refs_names_path(root, &key);
    let existed = path.exists();

    if path.exists() {
        fs::remove_file(&path)?;
    }
    if names_path.exists() {
        fs::remove_file(&names_path)?;
    }
    Ok(existed)
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

#[cfg(test)]
#[path = "refs_tests.rs"]
mod tests;
