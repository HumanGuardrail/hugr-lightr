//! hydrate: HydrateReport, hydrate_verified, hydrate, hydrate_impl.

use lightr_core::{Entry, LightrError, Manifest, Result};
use lightr_store::{CowRung, Store};
use rayon::prelude::*;
use std::{io, path::Path};

pub struct HydrateReport {
    pub root: lightr_core::Digest,
    pub files: u64,
    pub bytes_total: u64,
    pub rung: CowRung,
}

/// Verified hydrate: re-hash every object before materializing (paranoid
/// path; default `hydrate` trusts the sealed store — see ADR-0009).
pub fn hydrate_verified(dest: &Path, store: &Store, name: &str) -> Result<HydrateReport> {
    hydrate_impl(dest, store, name, true)
}

pub fn hydrate(dest: &Path, store: &Store, name: &str) -> Result<HydrateReport> {
    hydrate_impl(dest, store, name, false)
}

pub(super) fn hydrate_impl(
    dest: &Path,
    store: &Store,
    name: &str,
    verify: bool,
) -> Result<HydrateReport> {
    let rec = store
        .ref_get(name)?
        .ok_or_else(|| LightrError::RefNotFound(name.to_string()))?;

    let manifest_bytes = store.get_bytes(&rec.root)?;
    let manifest = Manifest::decode(&manifest_bytes)?;

    // dest must not exist OR be empty dir
    if dest.exists() {
        let is_empty = std::fs::read_dir(dest)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !is_empty {
            return Err(LightrError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination not empty",
            )));
        }
    }

    // Create dest
    std::fs::create_dir_all(dest).map_err(LightrError::Io)?;

    // Create all explicit Dir entries + parents of files/symlinks
    for entry in &manifest.entries {
        match entry {
            Entry::Dir { path } => {
                std::fs::create_dir_all(dest.join(path)).map_err(LightrError::Io)?;
            }
            Entry::File { path, .. } | Entry::Symlink { path, .. } => {
                if let Some(parent) = Path::new(path).parent() {
                    if parent.as_os_str().is_empty() {
                        // top-level: parent is dest
                    } else {
                        std::fs::create_dir_all(dest.join(parent))
                            .map_err(LightrError::Io)?;
                    }
                }
            }
        }
    }

    // Collect file and symlink entries separately
    let file_entries: Vec<&Entry> = manifest
        .entries
        .iter()
        .filter(|e| matches!(e, Entry::File { .. }))
        .collect();

    let symlink_entries: Vec<&Entry> = manifest
        .entries
        .iter()
        .filter(|e| matches!(e, Entry::Symlink { .. }))
        .collect();

    // Parallel materialize files — fail closed: first error aborts the report.
    // With `verify`, re-hash object bytes before materializing (the paranoid
    // path; the default trusts the sealed store — corruption is owned by
    // read paths, `--verify`, and fs-verity in R2).
    file_entries.par_iter().try_for_each(|e| {
        if let Entry::File {
            path, mode, digest, ..
        } = e
        {
            if verify {
                store.get_bytes(digest).map(|_| ())?;
            }
            store.materialize_file(digest, &dest.join(path), *mode)
        } else {
            Ok(())
        }
    })?;

    // Symlinks (sequential, cheap)
    for entry in &symlink_entries {
        if let Entry::Symlink { path, target } = entry {
            let link_path = dest.join(path);
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &link_path).map_err(LightrError::Io)?;
            // WIN-PATH: symlink creation on Windows requires Developer Mode or admin.
            // Best-effort: attempt symlink_file; fall back to fs::copy on error so
            // hydrate never hard-fails on a standard Windows installation.
            #[cfg(windows)]
            {
                let result = std::os::windows::fs::symlink_file(target, &link_path);
                if result.is_err() {
                    // Fall back: copy the target file if it exists.
                    let abs_target = if std::path::Path::new(target).is_absolute() {
                        std::path::PathBuf::from(target)
                    } else {
                        link_path.parent().unwrap_or(dest).join(target)
                    };
                    if abs_target.exists() {
                        std::fs::copy(&abs_target, &link_path).map_err(LightrError::Io)?;
                    }
                    // If target doesn't exist yet, skip silently (dangling symlink).
                }
            }
        }
    }

    let files = file_entries.len() as u64;
    let bytes_total = manifest.total_size;
    let rung = store.rung();

    Ok(HydrateReport {
        root: rec.root,
        files,
        bytes_total,
        rung,
    })
}
