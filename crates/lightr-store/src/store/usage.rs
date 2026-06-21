//! Read-only CAS disk-usage accounting for the diagnostic verbs
//! (`lightr info` / `lightr system df` — WP-EDGE-VERBS).
//!
//! Walks `<root>/objects/<2-hex>/<62-hex>` exactly as gc's count phase does,
//! but takes NO lock and mutates NOTHING: it only sums object sizes and counts
//! them. The shard/file shape match (2-char shard dir, 62-char object name) is
//! the same invariant gc relies on; non-conforming entries are skipped so a
//! stray file never inflates the report (fail-soft, never fatal).

use lightr_core::{LightrError, Result};
use std::path::Path;

/// Total on-disk CAS footprint: every object's count and summed byte length.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreUsage {
    /// Number of objects under `<root>/objects/`.
    pub objects: u64,
    /// Sum of every object file's length, in bytes.
    pub bytes: u64,
}

/// Accumulate [`StoreUsage`] by walking the objects tree under `root`.
///
/// Absent `objects/` ⇒ all-zero (a freshly-opened, never-written store). I/O
/// errors reading the top-level `objects/` dir are surfaced (the store is
/// expected to exist); per-entry metadata failures are treated as size 0
/// (fail-soft) so one unreadable object never aborts the whole report.
pub fn store_usage(root: &Path) -> Result<StoreUsage> {
    let objects_root = root.join("objects");
    let mut usage = StoreUsage::default();
    if !objects_root.exists() {
        return Ok(usage);
    }
    for shard_entry in std::fs::read_dir(&objects_root)
        .map_err(LightrError::Io)?
        .flatten()
    {
        let shard_path = shard_entry.path();
        if !shard_path.is_dir() {
            continue;
        }
        let shard_ok = shard_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|s| s.len() == 2);
        if !shard_ok {
            continue;
        }
        let inner = match std::fs::read_dir(&shard_path) {
            Ok(it) => it,
            Err(_) => continue, // shard vanished mid-walk; skip (fail-soft)
        };
        for obj_entry in inner.flatten() {
            let obj_path = obj_entry.path();
            if !obj_path.is_file() {
                continue;
            }
            let name_ok = obj_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s.len() == 62);
            if !name_ok {
                continue;
            }
            usage.objects += 1;
            usage.bytes += obj_path.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(usage)
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
