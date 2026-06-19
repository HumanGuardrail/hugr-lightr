//! timeaxis: DiffReport, diff_manifests, parse_lrr1, undo, bisect.

use super::gc::TempDirGuard;
use super::status::entries_differ;
use lightr_core::{Digest, Entry, LightrError, Manifest, RefRecord, Result};
use lightr_store::Store;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct DiffReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

// ---------------------------------------------------------------------------
// diff_manifests — path-sorted two-pointer merge
// ---------------------------------------------------------------------------

/// Compute the diff between two manifests (path-sorted merge).
/// added   = in new only
/// removed = in old only
/// changed = same path but (kind | digest | mode | symlink target) differ
pub fn diff_manifests(old: &Manifest, new: &Manifest) -> DiffReport {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    let old_entries = &old.entries;
    let new_entries = &new.entries;

    let mut oi = 0usize;
    let mut ni = 0usize;

    while oi < old_entries.len() || ni < new_entries.len() {
        match (old_entries.get(oi), new_entries.get(ni)) {
            (Some(oe), Some(ne)) => {
                let op = oe.path();
                let np = ne.path();
                match op.cmp(np) {
                    std::cmp::Ordering::Less => {
                        removed.push(op.to_string());
                        oi += 1;
                    }
                    std::cmp::Ordering::Greater => {
                        added.push(np.to_string());
                        ni += 1;
                    }
                    std::cmp::Ordering::Equal => {
                        if entries_differ(oe, ne) {
                            changed.push(op.to_string());
                        }
                        oi += 1;
                        ni += 1;
                    }
                }
            }
            (Some(oe), None) => {
                removed.push(oe.path().to_string());
                oi += 1;
            }
            (None, Some(ne)) => {
                added.push(ne.path().to_string());
                ni += 1;
            }
            (None, None) => break,
        }
    }

    DiffReport {
        added,
        removed,
        changed,
    }
}

// ---------------------------------------------------------------------------
// parse_lrr1
// ---------------------------------------------------------------------------

/// Parse an LRR1 AC value; returns (out_digest, err_digest) if valid.
/// LRR1 format: b"LRR1" [4] · exit_code_i32_le [4] · out_digest [32] · err_digest [32]
/// Total = 72 bytes.
pub fn parse_lrr1(bytes: &[u8]) -> Option<(Digest, Digest)> {
    if bytes.len() != 72 {
        return None;
    }
    if &bytes[..4] != b"LRR1" {
        return None;
    }
    // bytes[4..8] = exit_code i32 LE — not needed for mark
    let mut out_bytes = [0u8; 32];
    let mut err_bytes = [0u8; 32];
    out_bytes.copy_from_slice(&bytes[8..40]);
    err_bytes.copy_from_slice(&bytes[40..72]);
    Some((Digest(out_bytes), Digest(err_bytes)))
}

// ---------------------------------------------------------------------------
// undo
// ---------------------------------------------------------------------------

/// Re-point `name` to ref_log[1] (the previous version).
/// Errors RefNotFound if log has fewer than 2 entries.
pub fn undo(store: &Store, name: &str) -> Result<RefRecord> {
    let log = store.ref_log(name)?;
    if log.len() < 2 {
        return Err(LightrError::RefNotFound(name.to_string()));
    }
    let prev = log[1].clone();
    store.ref_put(&prev)?;
    Ok(prev)
}

// ---------------------------------------------------------------------------
// bisect
// ---------------------------------------------------------------------------

/// Binary-search the ref log to find the oldest-bad / newest-good boundary.
///
/// Assumes log[0] is the newest (bad) and log[n-1] is the oldest (good).
/// cmd exits 0 ⇒ good; exits ≠0 ⇒ bad.
/// Returns (first_bad_index, record) where first_bad_index is the
/// index of the oldest entry that is still bad (lo in the binary search).
pub fn bisect(store: &Store, name: &str, cmd: &[String]) -> Result<(usize, RefRecord)> {
    let log = store.ref_log(name)?;
    let n = log.len();
    if n < 2 {
        return Err(LightrError::InvalidRef(
            "bisect: need ≥2 versions".to_string(),
        ));
    }

    let test = |idx: usize| -> Result<bool> {
        // Hydrate log[idx] into a fresh tempdir.
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tmp_path =
            std::env::temp_dir().join(format!("lightr-bisect-{}-{}", pid, nanos));
        std::fs::create_dir_all(&tmp_path).map_err(LightrError::Io)?;
        let _guard = TempDirGuard(tmp_path.clone());

        // Hydrate the manifest into the tempdir.
        let manifest_bytes = store.get_bytes(&log[idx].root)?;
        let manifest = Manifest::decode(&manifest_bytes)?;
        for entry in &manifest.entries {
            match entry {
                Entry::File {
                    path, mode, digest, ..
                } => {
                    let dest = tmp_path.join(path);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
                    }
                    store.materialize_file(digest, &dest, *mode)?;
                }
                Entry::Symlink { path, target } => {
                    let link = tmp_path.join(path);
                    if let Some(parent) = link.parent() {
                        std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
                    }
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(target, &link).map_err(LightrError::Io)?;
                    // WIN-PATH: best-effort symlink; fall back to copy on failure.
                    #[cfg(windows)]
                    {
                        let result = std::os::windows::fs::symlink_file(target, &link);
                        if result.is_err() {
                            let abs_target = if std::path::Path::new(target).is_absolute() {
                                std::path::PathBuf::from(target)
                            } else {
                                link.parent().unwrap_or(&tmp_path).join(target)
                            };
                            if abs_target.exists() {
                                std::fs::copy(&abs_target, &link).map_err(LightrError::Io)?;
                            }
                        }
                    }
                }
                Entry::Dir { path } => {
                    std::fs::create_dir_all(tmp_path.join(path))
                        .map_err(LightrError::Io)?;
                }
            }
        }

        // Run the command in the tempdir.
        if cmd.is_empty() {
            return Err(LightrError::InvalidRef("bisect: empty cmd".to_string()));
        }
        let status = std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .current_dir(&tmp_path)
            .status()
            .map_err(LightrError::Io)?;

        // exit 0 ⇒ good (not bad); exit ≠0 ⇒ bad
        Ok(!status.success())
    };

    // Validate endpoints: log[0] must be bad, log[n-1] must be good.
    let end0_bad = test(0)?;
    let end_last_bad = test(n - 1)?;
    if !end0_bad || end_last_bad {
        return Err(LightrError::InvalidRef(
            "bisect: endpoints not bad/good".to_string(),
        ));
    }

    // Binary search: lo=0 (bad), hi=n-1 (good).
    // Invariant: log[lo] is bad, log[hi] is good.
    // Find the largest lo such that log[lo] is bad, log[lo+1] is good.
    let mut lo: usize = 0;
    let mut hi: usize = n - 1;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if test(mid)? {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    Ok((lo, log[lo].clone()))
}
