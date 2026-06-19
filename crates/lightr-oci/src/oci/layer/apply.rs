//! Layer application state machine.
//!
//! # FIX 3 + 4: Intra-layer whiteout ordering
//!
//! OCI spec: whiteout entries in a layer refer to the *parent* layer's
//! contents. Within a single layer we process ALL deletes (whiteouts) before
//! any additions so that a file added AND whited out in the same layer ends up
//! absent (OCI parent-ref semantics).
//!
//! Implementation: two-pass per layer.
//!   Pass 1 (`collect_ops`) — parse tar into three buckets:
//!     `dirs`      — directory entries (create first, before any writes)
//!     `whiteouts` — (parent_in_temp, whiteout_name or None for opaque)
//!     `pending`   — regular files, symlinks, hardlinks
//!   Pass 2 (`apply_ops`) — apply dirs → whiteouts → regular/symlink → hardlinks.
//!   FIX 5: hardlinks resolved after all regular files are written.

use super::super::util::path_is_safe;
use lightr_core::{LightrError, Result};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    time::Instant,
};

// ─────────────────────────────────────────────────────────────────────────────
// Data types: shared between collect_ops and apply_ops
// ─────────────────────────────────────────────────────────────────────────────

/// A pending whiteout delete collected during Pass 1.
pub(super) struct WhiteoutOp {
    pub(super) parent: PathBuf,
    /// `Some(name)` ⇒ delete that name; `None` ⇒ opaque (clear all children).
    pub(super) name: Option<String>,
}

/// A pending file or symlink write collected during Pass 1.
pub(super) enum PendingEntry {
    Regular {
        dest: PathBuf,
        data: Vec<u8>,
        mode: u32,
    },
    Symlink {
        dest: PathBuf,
        link_target: PathBuf,
    },
    /// A hardlink: `dest` should be a copy of `src` (both relative to tempdir
    /// but `src` is the as-declared path from the tar header, still needs
    /// resolving against tempdir).
    Hardlink {
        dest: PathBuf,
        /// The declared target path from the tar header (NOT yet joined with
        /// tempdir; we resolve it after all regular files are written).
        declared_target: PathBuf,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Pass 1: parse tar → buckets
// ─────────────────────────────────────────────────────────────────────────────

/// Parse an open tar `Archive` into operation buckets.
///
/// Samples `deadline` every 256 entries; returns
/// `(dirs, whiteouts, pending, whited_out_paths)`.
/// Increments the shared `entry_count` and `skipped` counters.
/// The four buckets produced by [`collect_ops`]: dirs to create, whiteouts to
/// apply, pending file/symlink/hardlink entries, and the set of whited-out paths.
type CollectedOps = (
    Vec<PathBuf>,
    Vec<WhiteoutOp>,
    Vec<PendingEntry>,
    HashSet<PathBuf>,
);

pub(super) fn collect_ops<R: Read>(
    archive: &mut tar::Archive<R>,
    tempdir: &Path,
    deadline: Instant,
    entry_count: &mut u64,
    skipped: &mut u64,
    timeout_secs: u64,
) -> Result<CollectedOps> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut whiteouts: Vec<WhiteoutOp> = Vec::new();
    let mut pending: Vec<PendingEntry> = Vec::new();
    // whited_out_paths: absolute paths (within tempdir) that must be absent
    // after this layer — even if the same layer also adds them (whiteout wins).
    let mut whited_out_paths: HashSet<PathBuf> = HashSet::new();

    for entry_result in archive.entries().map_err(LightrError::Io)? {
        // Deadline check: sample every 256 entries to keep hot-path
        // overhead ~zero.  Fail closed with InvalidManifest on exceed.
        *entry_count += 1;
        if *entry_count & 0xFF == 0 && Instant::now() >= deadline {
            return Err(LightrError::InvalidManifest(format!(
                "layer extraction timed out after {} s (LIGHTR_LAYER_TIMEOUT_SECS)",
                timeout_secs
            )));
        }

        let mut entry = entry_result.map_err(LightrError::Io)?;
        let entry_path = entry.path().map_err(LightrError::Io)?.into_owned();

        // Path safety: reject `..` or absolute entries
        if !path_is_safe(&entry_path) {
            *skipped += 1;
            continue;
        }

        // Strip a leading `.` component (common in OCI layers)
        let rel: PathBuf = entry_path
            .components()
            .skip_while(|c| matches!(c, Component::CurDir))
            .collect();

        if rel.as_os_str().is_empty() {
            continue;
        }

        let file_name = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let parent_in_temp = if let Some(p) = rel.parent() {
            tempdir.join(p)
        } else {
            tempdir.to_path_buf()
        };

        use tar::EntryType;
        match entry.header().entry_type() {
            EntryType::Directory => {
                // OCI whiteout files are sometimes emitted as Directory-type entries
                // (e.g. by the `make_layer` fixture and some OCI producers). We must
                // check for whiteout names BEFORE treating the entry as a directory.
                // FIX 4 (opaque whiteout via dir entry)
                if file_name == ".wh..wh..opq" {
                    whiteouts.push(WhiteoutOp {
                        parent: parent_in_temp,
                        name: None, // opaque
                    });
                    continue;
                }
                // FIX 3 (whiteout via dir entry)
                if let Some(whiteout_name) = file_name.strip_prefix(".wh.") {
                    whited_out_paths.insert(parent_in_temp.join(whiteout_name));
                    whiteouts.push(WhiteoutOp {
                        parent: parent_in_temp,
                        name: Some(whiteout_name.to_string()),
                    });
                    continue;
                }
                dirs.push(tempdir.join(&rel));
            }
            EntryType::Regular | EntryType::Continuous => {
                // FIX 4 (opaque whiteout): `.wh..wh..opq` → clear the dir
                if file_name == ".wh..wh..opq" {
                    whiteouts.push(WhiteoutOp {
                        parent: parent_in_temp,
                        name: None, // opaque
                    });
                    continue;
                }
                // FIX 3 (regular whiteout): `.wh.<name>` → delete <name>
                if let Some(whiteout_name) = file_name.strip_prefix(".wh.") {
                    // Track this as a path that must be absent after this layer
                    // (even if the layer also adds this exact path — whiteout wins).
                    whited_out_paths.insert(parent_in_temp.join(whiteout_name));
                    whiteouts.push(WhiteoutOp {
                        parent: parent_in_temp,
                        name: Some(whiteout_name.to_string()),
                    });
                    continue;
                }
                // Regular file: collect content
                let dest = tempdir.join(&rel);
                let mode = entry.header().mode().map_err(LightrError::Io)?;
                let mut data = Vec::new();
                entry.read_to_end(&mut data).map_err(LightrError::Io)?;
                pending.push(PendingEntry::Regular { dest, data, mode });
            }
            EntryType::Symlink => {
                let dest = tempdir.join(&rel);
                let link_target = entry
                    .header()
                    .link_name()
                    .map_err(LightrError::Io)?
                    .map(|p| p.into_owned())
                    .unwrap_or_else(|| PathBuf::from(""));
                pending.push(PendingEntry::Symlink { dest, link_target });
            }
            EntryType::Link => {
                // FIX 5: Hardlink — collect for second pass; missing target ⇒ error.
                let dest = tempdir.join(&rel);
                let link_target = entry
                    .header()
                    .link_name()
                    .map_err(LightrError::Io)?
                    .map(|p| p.into_owned())
                    .unwrap_or_else(|| PathBuf::from(""));
                // Strip leading ./ from the declared target
                let clean_target: PathBuf = link_target
                    .components()
                    .skip_while(|c| matches!(c, Component::CurDir))
                    .collect();
                pending.push(PendingEntry::Hardlink {
                    dest,
                    declared_target: clean_target,
                });
            }
            _ => {
                // Other entry types (char/block devices, fifos) — skip
            }
        }
    }

    Ok((dirs, whiteouts, pending, whited_out_paths))
}

// ─────────────────────────────────────────────────────────────────────────────
// Pass 2: apply dirs → whiteouts → files/symlinks → hardlinks
// ─────────────────────────────────────────────────────────────────────────────

/// Apply the collected operation buckets to `tempdir`.
///
/// Order: dirs → whiteouts (FIX 3 + 4) → regular files / symlinks →
/// hardlinks (FIX 5).
pub(super) fn apply_ops(
    tempdir: &Path,
    dirs: &[PathBuf],
    whiteouts: &[WhiteoutOp],
    pending: &[PendingEntry],
    whited_out_paths: &HashSet<PathBuf>,
) -> Result<()> {
    // ── Apply directories first ───────────────────────────────────────────
    for dir_path in dirs {
        fs::create_dir_all(dir_path).map_err(LightrError::Io)?;
    }

    // ── Apply whiteouts (ALL before additions — FIX 3 + 4) ───────────────
    for wo in whiteouts {
        match &wo.name {
            // Regular whiteout: `.wh.<name>` — remove `<name>`
            Some(name) => {
                let target = wo.parent.join(name);
                if target.is_dir() {
                    let _ = fs::remove_dir_all(&target);
                } else {
                    let _ = fs::remove_file(&target);
                }
            }
            // Opaque whiteout: clear the dir's existing contents (keep dir).
            // FIX 4: create the dir if it is absent, THEN clear it.
            None => {
                fs::create_dir_all(&wo.parent).map_err(LightrError::Io)?;
                for child in fs::read_dir(&wo.parent).map_err(LightrError::Io)?.flatten() {
                    let cp = child.path();
                    if cp.is_dir() {
                        let _ = fs::remove_dir_all(&cp);
                    } else {
                        let _ = fs::remove_file(&cp);
                    }
                }
            }
        }
    }

    // ── Apply regular files and symlinks ──────────────────────────────────
    // Skip any file whose absolute dest path is in whited_out_paths (FIX 3:
    // whiteout wins even for same-layer adds). Also skip files inside opaque-
    // whiteout dirs that were not added by this layer (already cleared above).
    //
    // Hardlinks are deferred until after regular files are written so that
    // a hardlink target that appears earlier in the layer has been written.
    for pe in pending {
        match pe {
            PendingEntry::Regular { dest, data, mode } => {
                // Whiteout wins: skip if this path was whited out in this layer.
                if whited_out_paths.contains(dest.as_path()) {
                    continue;
                }
                if let Some(p) = dest.parent() {
                    fs::create_dir_all(p).map_err(LightrError::Io)?;
                }
                fs::write(dest, data).map_err(LightrError::Io)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(dest, fs::Permissions::from_mode(*mode))
                        .map_err(LightrError::Io)?;
                }
                #[cfg(windows)]
                {
                    // WIN-PATH: Windows has no POSIX mode bits; honour read-only (bit 0o200 = owner write).
                    // All other permission semantics are skipped on Windows.
                    let readonly = (*mode & 0o200) == 0;
                    if readonly {
                        let mut perms =
                            fs::metadata(dest).map_err(LightrError::Io)?.permissions();
                        perms.set_readonly(true);
                        let _ = fs::set_permissions(dest, perms);
                    }
                }
            }
            PendingEntry::Symlink { dest, link_target } => {
                if whited_out_paths.contains(dest.as_path()) {
                    continue;
                }
                if let Some(p) = dest.parent() {
                    fs::create_dir_all(p).map_err(LightrError::Io)?;
                }
                let _ = fs::remove_file(dest);
                #[cfg(unix)]
                std::os::unix::fs::symlink(link_target, dest).map_err(LightrError::Io)?;
                #[cfg(windows)]
                {
                    // WIN-PATH: symlink creation requires Developer Mode or admin on Windows.
                    // Fall back to copying the target if symlink creation fails so import never hard-fails.
                    use std::os::windows::fs::symlink_file;
                    if symlink_file(link_target, dest).is_err() {
                        // Symlink creation failed (no Dev Mode / not admin) — copy the target instead.
                        // The target may itself be relative; resolve it against dest's parent.
                        let resolved_target = if link_target.is_absolute() {
                            link_target.to_path_buf()
                        } else {
                            dest.parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .join(link_target)
                        };
                        if resolved_target.exists() {
                            fs::copy(&resolved_target, dest).map_err(LightrError::Io)?;
                        }
                        // If target does not exist either (broken symlink in the layer), skip — no error.
                    }
                }
            }
            PendingEntry::Hardlink { .. } => {} // handled below
        }
    }

    // ── Resolve hardlinks (FIX 5) ─────────────────────────────────────────
    // All regular files in this layer are now written. Attempt to resolve
    // each hardlink; if the target is still missing ⇒ error (fail-closed).
    for pe in pending {
        if let PendingEntry::Hardlink {
            dest,
            declared_target,
        } = pe
        {
            // Whiteout also covers hardlink destinations.
            if whited_out_paths.contains(dest.as_path()) {
                continue;
            }
            let src = tempdir.join(declared_target);
            if !src.exists() {
                return Err(LightrError::InvalidManifest(format!(
                    "hardlink target not found: {}",
                    declared_target.display()
                )));
            }
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p).map_err(LightrError::Io)?;
            }
            fs::copy(&src, dest).map_err(LightrError::Io)?;
        }
    }

    Ok(())
}
