//! snapshot: SnapshotReport, snapshot.

use super::{codec::Index, scan::scan};
use lightr_core::{Entry, RefRecord, Result};
use lightr_store::Store;
use rayon::prelude::*;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct SnapshotReport {
    pub root: lightr_core::Digest,
    pub files: u64,
    pub bytes_total: u64,
    pub objects_new: u64,
}

pub fn snapshot(root: &Path, store: &Store, name: &str) -> Result<SnapshotReport> {
    lightr_core::validate_ref_name(name)?;

    let prev = store.ref_get(name)?;
    let mut index = Index::load_for(root)?;
    let walk = scan(root, &mut index)?;
    let manifest = walk.manifest;

    // Collect file entries that need ingestion
    let file_entries: Vec<&Entry> = manifest
        .entries
        .iter()
        .filter(|e| matches!(e, Entry::File { .. }))
        .collect();

    // Parallel ingest of missing objects
    let ingest_results: Vec<(lightr_core::Digest, bool)> = file_entries
        .par_iter()
        .filter_map(|e| {
            if let Entry::File { digest, .. } = e {
                if !store.exists(digest) {
                    // Find the file path on disk
                    let rel = e.path();
                    let abs = root.join(rel);
                    match store.ingest_file(&abs) {
                        Ok(_) => Some((*digest, true)),
                        Err(_) => None,
                    }
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    let objects_new = ingest_results.len() as u64;

    // Encode and store manifest
    let manifest_bytes = manifest.encode();
    store.put_bytes(&manifest_bytes)?;
    let manifest_digest = manifest.digest();

    // Build ref record
    let parent = prev.map(|r| r.root);
    let created_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let rec = RefRecord {
        name: name.to_string(),
        root: manifest_digest,
        parent,
        created_at_unix,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    store.ref_put(&rec)?;

    let files = file_entries.len() as u64;
    let bytes_total = manifest.total_size;

    Ok(SnapshotReport {
        root: manifest_digest,
        files,
        bytes_total,
        objects_new,
    })
}
