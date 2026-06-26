//! Walk / scan: WalkReport, WalkCandidate, stat_fields, scan.

use super::codec::{Index, IndexEntry};
use lightr_core::{Digest, Entry, LightrError, Manifest, Result};
use rayon::prelude::*;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub struct WalkReport {
    pub manifest: Manifest,
    pub rehashed: u64,
    pub from_index: u64,
}

/// Walk candidate collected during the sequential directory walk.
#[derive(Debug)]
pub(super) struct WalkCandidate {
    /// Relative path in the manifest (forward-slash separated, sorted).
    pub(super) rel_path: String,
    pub(super) abs_path: PathBuf,
    pub(super) kind: u8, // 0=File, 1=Symlink, 2=Dir
    pub(super) mode: u32,
    pub(super) size: u64,
    pub(super) mtime_ns: u64,
    pub(super) ino: u64,
    /// Symlink target, if any.
    pub(super) symlink_target: Option<String>,
    /// Digest from index (if matched).
    pub(super) cached_digest: Option<Digest>,
}

/// Returns (mtime_ns, ino, size, mode) from a symlink_metadata result.
pub(super) fn stat_fields(meta: &std::fs::Metadata) -> (u64, u64, u64, u32) {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // inode number: unix-only. On Windows, use 0 (index uses mtime+size for
    // change detection; ino is an optimization hint, not required for correctness).
    #[cfg(unix)]
    let ino = meta.ino();
    #[cfg(windows)]
    let ino = 0u64;
    let size = meta.len();
    // full mode including type bits masked to permissions (unix).
    // On Windows, mode bits are not meaningful — use a conventional default.
    #[cfg(unix)]
    let mode = meta.permissions().mode() & 0o7777;
    #[cfg(windows)]
    let mode = if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    };
    (mtime_ns, ino, size, mode)
}

pub fn scan(root: &Path, index: &mut Index) -> Result<WalkReport> {
    use ignore::WalkBuilder;

    let canonical_root = root.canonicalize().map_err(LightrError::Io)?;

    // Collect walk candidates sequentially (ignore::Walk isn't Send easily)
    let mut candidates: Vec<WalkCandidate> = Vec::new();

    let walker = WalkBuilder::new(&canonical_root)
        .hidden(false) // include dotfiles
        .ignore(true) // respect .gitignore
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .add_custom_ignore_filename(".lightrignore")
        .filter_entry(|entry| {
            // Explicitly skip ".git" dir at any depth
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                return entry.file_name() != ".git";
            }
            true
        })
        .build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue, // ignore walk errors
        };

        let abs_path = entry.path().to_path_buf();

        // Skip the root itself
        if abs_path == canonical_root {
            continue;
        }

        let meta = match abs_path.symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let (mtime_ns, ino, size, raw_mode) = stat_fields(&meta);

        // Relative path: forward-slash, relative to root
        let rel = abs_path
            .strip_prefix(&canonical_root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");

        if meta.is_symlink() {
            let target = std::fs::read_link(&abs_path)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            candidates.push(WalkCandidate {
                rel_path: rel,
                abs_path,
                kind: 1,
                mode: raw_mode,
                size: 0,
                mtime_ns,
                ino,
                symlink_target: Some(target),
                cached_digest: None,
            });
        } else if meta.is_dir() {
            // Record a directory entry when it is EMPTY (a non-empty dir is implied
            // by its children — hydrate re-creates parents via create_dir_all) OR
            // when we cannot read it. The old `unwrap_or(false)` treated an
            // UNREADABLE dir (e.g. mode-700 `/root` scanned by a non-root process)
            // as non-empty and SILENTLY DROPPED it (fail-open) — so the path
            // vanished from the materialized rootfs. Recording it preserves the
            // path; unreadable *children* still require snapshotting with read
            // access (you cannot capture what you cannot read — a usage rule).
            let record_dir = match std::fs::read_dir(&abs_path) {
                Ok(mut d) => d.next().is_none(), // readable + empty → record the dir
                Err(_) => true,                  // unreadable → record so the path survives
            };
            if record_dir {
                candidates.push(WalkCandidate {
                    rel_path: rel,
                    abs_path,
                    kind: 2,
                    mode: raw_mode,
                    size: 0,
                    mtime_ns,
                    ino,
                    symlink_target: None,
                    cached_digest: None,
                });
            }
        } else if meta.is_file() {
            // Check index cache
            let cached_digest = index.get(&rel).and_then(|ie| {
                // Racily-clean: if mtime == saved_at_ns, must rehash
                if ie.size == size
                    && ie.mtime_ns == mtime_ns
                    && ie.ino == ino
                    && mtime_ns < index.saved_at_ns
                {
                    Some(ie.digest)
                } else {
                    None
                }
            });

            candidates.push(WalkCandidate {
                rel_path: rel,
                abs_path,
                kind: 0,
                mode: raw_mode,
                size,
                mtime_ns,
                ino,
                symlink_target: None,
                cached_digest,
            });
        }
    }

    // Sort by path for deterministic manifest
    candidates.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // Identify files needing hashing
    let needs_hash: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == 0 && c.cached_digest.is_none())
        .map(|(i, _)| i)
        .collect();

    // Parallel hash of files that need it
    let hashes: Vec<(usize, Result<Digest>)> = needs_hash
        .par_iter()
        .map(|&i| {
            let c = &candidates[i];
            let d = Digest::of_file(&c.abs_path);
            (i, d)
        })
        .collect();

    let mut rehashed = 0u64;
    let mut from_index = 0u64;

    // Apply hashes back
    let mut hash_results: HashMap<usize, Digest> = HashMap::new();
    for (i, res) in hashes {
        if let Ok(d) = res {
            hash_results.insert(i, d);
            rehashed += 1;
        }
    }

    // Count from_index
    for c in &candidates {
        if c.kind == 0 && c.cached_digest.is_some() {
            from_index += 1;
        }
    }

    // Build manifest entries and update index
    let mut total_size: u64 = 0;
    let mut entries: Vec<Entry> = Vec::new();

    for (i, c) in candidates.iter().enumerate() {
        match c.kind {
            0 => {
                // File
                let digest = if let Some(d) = c.cached_digest {
                    d
                } else if let Some(&d) = hash_results.get(&i) {
                    d
                } else {
                    continue; // skip unhashable files
                };

                total_size += c.size;
                entries.push(Entry::File {
                    path: c.rel_path.clone(),
                    mode: c.mode,
                    size: c.size,
                    digest,
                });

                // Update index
                index.upsert(IndexEntry {
                    kind: 0,
                    mode: c.mode,
                    size: c.size,
                    mtime_ns: c.mtime_ns,
                    ino: c.ino,
                    digest,
                    path: c.rel_path.clone(),
                });
            }
            1 => {
                // Symlink
                let target = c.symlink_target.clone().unwrap_or_default();
                entries.push(Entry::Symlink {
                    path: c.rel_path.clone(),
                    target,
                });
            }
            2 => {
                // Empty dir
                entries.push(Entry::Dir {
                    path: c.rel_path.clone(),
                });
            }
            _ => {}
        }
    }

    // Save updated index
    index.save_for(root)?;

    let manifest = Manifest {
        version: 1,
        total_size,
        entries,
    };

    Ok(WalkReport {
        manifest,
        rehashed,
        from_index,
    })
}
