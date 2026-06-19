//! status: StatusReport, status, entries_differ.

use super::{codec::Index, scan::scan, timeaxis::diff_manifests};
use lightr_core::{Entry, LightrError, Result};
use lightr_store::Store;
use std::path::Path;

pub struct StatusReport {
    pub clean: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

pub fn status(root: &Path, store: &Store, name: &str) -> Result<StatusReport> {
    let rec = store
        .ref_get(name)?
        .ok_or_else(|| LightrError::RefNotFound(name.to_string()))?;

    let manifest_bytes = store.get_bytes(&rec.root)?;
    let remote_manifest = lightr_core::Manifest::decode(&manifest_bytes)?;

    let mut index = Index::load_for(root)?;
    let walk = scan(root, &mut index)?;
    let local_manifest = walk.manifest;

    // Delegate to diff_manifests (defined in timeaxis).
    // old = remote (stored), new = local (working tree).
    let diff = diff_manifests(&remote_manifest, &local_manifest);

    let clean = diff.added.is_empty() && diff.removed.is_empty() && diff.changed.is_empty();

    Ok(StatusReport {
        clean,
        added: diff.added,
        removed: diff.removed,
        changed: diff.changed,
    })
}

/// Returns true if two entries with the same path differ in a meaningful way.
pub(crate) fn entries_differ(remote: &Entry, local: &Entry) -> bool {
    match (remote, local) {
        (
            Entry::File {
                digest: rd,
                mode: rm,
                ..
            },
            Entry::File {
                digest: ld,
                mode: lm,
                ..
            },
        ) => rd != ld || rm != lm,
        (Entry::Symlink { target: rt, .. }, Entry::Symlink { target: lt, .. }) => rt != lt,
        (Entry::Dir { .. }, Entry::Dir { .. }) => false,
        // Different kinds at same path
        _ => true,
    }
}
