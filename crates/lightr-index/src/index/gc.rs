//! gc: GcReport, gc, TempDirGuard (+Drop).

use super::timeaxis::parse_lrr1;
use lightr_core::{Entry, LightrError, Manifest, Result};
use lightr_store::Store;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct GcReport {
    pub objects_total: u64,
    pub reachable: u64,
    pub swept: u64,
    pub bytes_freed: u64,
    pub run_dirs_removed: u64,
}

/// Guard struct: removes the tempdir in all paths (drop on success or panic).
pub(crate) struct TempDirGuard(pub(crate) std::path::PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// GC: mark all reachable objects, sweep unreachable ones; prune stale run dirs.
///
/// dry_run=true  → count only, no mutations.
/// dry_run=false → remove unreachable objects and stale run dirs.
pub fn gc(store: &Store, dry_run: bool, min_age_secs: u64) -> Result<GcReport> {
    use std::collections::HashSet;

    let _g = store.gc_guard()?;

    let mut mark: HashSet<lightr_core::Digest> = HashSet::new();

    // --- Mark phase: ref-log manifests + file entries ---
    for name in store.list_refs()? {
        for rec in store.ref_log(&name)? {
            // Attempt to decode the manifest; skip if corrupt/missing.
            let manifest_bytes = match store.get_bytes(&rec.root) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let manifest = match Manifest::decode(&manifest_bytes) {
                Ok(m) => m,
                Err(_) => continue,
            };
            mark.insert(rec.root);
            for entry in &manifest.entries {
                if let Entry::File { digest, .. } = entry {
                    mark.insert(*digest);
                }
            }
        }
    }

    // --- Mark phase: AC records (LRR1 entries) ---
    for value in store.list_ac()? {
        if let Some((out_d, err_d)) = parse_lrr1(&value) {
            mark.insert(out_d);
            mark.insert(err_d);
        }
    }

    // --- Count objects + find sweep candidates ---
    let objects_root = store.root().join("objects");
    let mut objects_total: u64 = 0;
    let mut sweep_candidates: Vec<(lightr_core::Digest, u64)> = Vec::new(); // (digest, size)

    if objects_root.exists() {
        for shard_entry in std::fs::read_dir(&objects_root)
            .map_err(LightrError::Io)?
            .flatten()
        {
            let shard_path = shard_entry.path();
            if !shard_path.is_dir() {
                continue;
            }
            let shard_prefix = shard_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if shard_prefix.len() != 2 {
                continue;
            }
            for obj_entry in std::fs::read_dir(&shard_path)
                .map_err(LightrError::Io)?
                .flatten()
            {
                let obj_path = obj_entry.path();
                if !obj_path.is_file() {
                    continue;
                }
                let rest = obj_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if rest.len() != 62 {
                    continue;
                }
                objects_total += 1;
                let hex = format!("{}{}", shard_prefix, rest);
                if let Ok(d) = lightr_core::Digest::from_hex(&hex) {
                    if !mark.contains(&d) {
                        let size = obj_path.metadata().map(|m| m.len()).unwrap_or(0);
                        sweep_candidates.push((d, size));
                    }
                }
            }
        }
    }

    let reachable = objects_total.saturating_sub(sweep_candidates.len() as u64);
    let swept_count = sweep_candidates.len() as u64;
    let mut bytes_freed: u64 = 0;

    if !dry_run {
        for (d, size) in &sweep_candidates {
            if store.remove_object(d).is_ok() {
                bytes_freed += size;
            }
        }
    }

    // --- Run dirs: prune stale exited dirs ---
    let lightr_home = std::env::var("LIGHTR_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
                .join(".lightr")
        });

    let run_root = lightr_home.join("run");
    let mut run_dirs_removed: u64 = 0;

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Hard-killed runs leave status=="running" forever — reclaim them via a
    // coarse-age heuristic.  We cannot use kill(pid, 0) because this crate
    // forbids unsafe code and libc is not a dependency.  Instead we treat a
    // run dir as provably-dead when ALL of:
    //   1. status does NOT start with "exited" (still shows "running" or is absent)
    //   2. a `pid` file is present in the dir (written by lightr-run)
    //   3. dir mtime is older than now − HARD_KILLED_STALE_SECS (24 h default)
    //   4. dir mtime is also older than now − min_age_secs (caller's preference)
    // If no pid file exists we leave the dir alone (conservative).
    const HARD_KILLED_STALE_SECS: u64 = 24 * 3600;

    if run_root.exists() {
        for dir_entry in std::fs::read_dir(&run_root)
            .map_err(LightrError::Io)?
            .flatten()
        {
            let dir_path = dir_entry.path();
            if !dir_path.is_dir() {
                continue;
            }
            // Shared: read status and dir mtime up front — used by both branches.
            let status_path = dir_path.join("status");
            let status_str = std::fs::read_to_string(&status_path).unwrap_or_default();
            let mtime_secs = dir_path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let age_secs = now_secs.saturating_sub(mtime_secs);

            if status_str.starts_with("exited") {
                // ── Normal clean-exit path ────────────────────────────────────
                if age_secs <= min_age_secs {
                    continue;
                }
                run_dirs_removed += 1;
                if !dry_run {
                    let _ = std::fs::remove_dir_all(&dir_path);
                }
            } else {
                // ── Hard-killed / stuck "running" path ────────────────────────
                // Only reclaim if a pid file exists (proof that a run was started)
                // AND the dir is older than both the caller's min_age and the
                // hard-killed floor (24 h).  NEVER reclaim if pid file is absent.
                let pid_path = dir_path.join("pid");
                if !pid_path.exists() {
                    continue;
                }
                let effective_min = min_age_secs.max(HARD_KILLED_STALE_SECS);
                if age_secs <= effective_min {
                    continue;
                }
                run_dirs_removed += 1;
                if !dry_run {
                    let _ = std::fs::remove_dir_all(&dir_path);
                }
            }
        }
    }

    Ok(GcReport {
        objects_total,
        reachable,
        swept: swept_count,
        bytes_freed,
        run_dirs_removed,
    })
}
