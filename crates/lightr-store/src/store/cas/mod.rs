//! CAS object plane — put_bytes / ingest_file / get_bytes / materialize_file.
//!
//! Objects are content-addressed (blake3), sharded 2/62, stored read-only (0o444).
//! Writes go through a temp+rename+fsync pipeline for crash durability.

use super::cow::{cow_copy_file, try_cow_at_rung, CowRung};
use super::lock::write_guard;
use lightr_core::{Digest, LightrError, Result};
#[cfg(unix)]
use std::fs::Permissions;
use std::fs::{self, File};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ── path helpers ──────────────────────────────────────────────────────────────

/// Returns the two-char shard prefix and 62-char remainder from a Digest hex.
pub(super) fn shard_parts(hex: &str) -> (&str, &str) {
    (&hex[..2], &hex[2..])
}

/// Object path: <root>/objects/<2hex>/<62hex>
pub(crate) fn object_path(root: &Path, d: &Digest) -> PathBuf {
    let hex = d.to_hex();
    let (pre, rest) = shard_parts(&hex);
    root.join("objects").join(pre).join(rest)
}

/// A cheap nonce for temp file names: PID + digest-hex-prefix + nanos.
pub(super) fn temp_suffix(hint: &str) -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{pid}-{hint}-{nanos}")
}

/// fsync the parent directory so the rename (directory entry change) is
/// crash-durable on macOS/Linux.
///
/// On Windows: NTFS has no portable directory fsync API (FlushFileBuffers on a
/// directory handle is not guaranteed to flush directory metadata to disk across
/// all NTFS configurations). This function is a documented no-op on Windows.
/// The weaker guarantee: file data and the rename are durable once
/// FlushFileBuffers is called on the FILE itself (done in atomic_write before
/// rename), but the directory entry update may not be crash-synced.
/// This is acceptable for the CAS store (objects are content-addressed;
/// a missing directory entry after a crash means re-ingest, not corruption).
pub(super) fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let f = File::open(dir)?;
        f.sync_all()?;
    }
    // Windows: documented no-op — see function doc above.
    #[cfg(windows)]
    let _ = dir;
    Ok(())
}

/// Atomic write: write `data` to a temp file in `parent`, fsync the file,
/// rename to `dest`, then fsync the parent directory so the rename is
/// crash-durable.
pub(super) fn atomic_write(parent: &Path, dest: &Path, data: &[u8]) -> Result<()> {
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".tmp-{}", temp_suffix("w")));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        // fsync before rename: flush file data to stable storage.
        #[cfg(unix)]
        f.sync_all()?;
        #[cfg(windows)]
        {
            // WIN-PATH: FlushFileBuffers ensures data is on disk before rename.
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;
            let handle = f.as_raw_handle();
            unsafe { FlushFileBuffers(handle as _) };
        }
    }
    fs::rename(&tmp, dest)?;
    fsync_dir(parent)?; // fsync parent dir after rename
    Ok(())
}

/// chmod a path to the given mode bits (unix only).
pub(super) fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, Permissions::from_mode(mode))?;
    }
    // Windows: mode bits are a Unix concept. Skip silently.
    // Windows uses ACLs/read-only attribute semantics — not set here.
    #[cfg(windows)]
    {
        let _ = (path, mode);
    }
    Ok(())
}

// ── CAS methods (called from Store) ─────────────────────────────────────────

/// Content-address `bytes` and store them.  Idempotent: if the object
/// already exists the digest is returned immediately without any write.
pub fn put_bytes(root: &Path, bytes: &[u8]) -> Result<Digest> {
    let _wg = write_guard(root)?;
    let d = Digest::of_bytes(bytes);
    let path = object_path(root, &d);

    if path.exists() {
        return Ok(d);
    }

    let hex = d.to_hex();
    let (pre, _) = shard_parts(&hex);
    let shard = root.join("objects").join(pre);
    fs::create_dir_all(&shard)?;

    let tmp_name = format!(".tmp-{}", temp_suffix(&hex[..8]));
    let tmp = shard.join(tmp_name);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        // fsync before rename: flush file data.
        #[cfg(unix)]
        f.sync_all()?;
        #[cfg(windows)]
        {
            // WIN-PATH: FlushFileBuffers on the file before rename.
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;
            let handle = f.as_raw_handle();
            unsafe { FlushFileBuffers(handle as _) };
        }
    }
    fs::rename(&tmp, &path)?;
    fsync_dir(&shard)?; // fsync parent dir after rename
    set_mode(&path, 0o444)?;

    Ok(d)
}

/// Hash `path` and CoW-clone it into the store.  Idempotent.
pub fn ingest_file(root: &Path, path: &Path, rung: CowRung) -> Result<Digest> {
    let _wg = write_guard(root)?;
    let d = Digest::of_file(path)?;
    let dest = object_path(root, &d);

    if dest.exists() {
        return Ok(d);
    }

    let hex = d.to_hex();
    let (pre, _) = shard_parts(&hex);
    let shard = root.join("objects").join(pre);
    fs::create_dir_all(&shard)?;

    let tmp_name = format!(".tmp-{}", temp_suffix(&hex[..8]));
    let tmp = shard.join(tmp_name);

    // Try CoW into a temp, then rename+chmod.
    // On failure fall through to fs::copy.
    let used_cow = match try_cow_at_rung(path, &tmp, rung) {
        Ok(()) => true,
        Err(_) => {
            let _ = fs::remove_file(&tmp);
            fs::copy(path, &tmp)?;
            false
        }
    };
    let _ = used_cow; // counted but not surfaced in API

    // fsync the temp file before rename so the data is crash-durable.
    {
        let f = File::open(&tmp)?;
        #[cfg(unix)]
        f.sync_all()?;
        #[cfg(windows)]
        {
            // WIN-PATH: FlushFileBuffers on the temp file before rename.
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;
            let handle = f.as_raw_handle();
            unsafe { FlushFileBuffers(handle as _) };
        }
    }
    fs::rename(&tmp, &dest)?;
    fsync_dir(&shard)?; // fsync parent dir after rename
    set_mode(&dest, 0o444)?;

    Ok(d)
}

/// Read and verify `d`.  Missing → NotFound.  Hash mismatch → Integrity
/// (evidence file kept, never deleted).
pub fn get_bytes(root: &Path, d: &Digest) -> Result<Vec<u8>> {
    let path = object_path(root, d);
    if !path.exists() {
        return Err(LightrError::NotFound(*d));
    }
    let bytes = fs::read(&path)?;
    let actual = Digest::of_bytes(&bytes);
    if actual != *d {
        return Err(LightrError::Integrity {
            expected: *d,
            actual,
        });
    }
    Ok(bytes)
}

/// Returns true iff the object file exists (no rehash).
pub fn exists(root: &Path, d: &Digest) -> bool {
    object_path(root, d).exists()
}

/// CoW the object identified by `d` to `dest`, then set its mode to
/// `mode`.  Missing object → NotFound.  Parent dirs created if absent.
pub fn materialize_file(
    root: &Path,
    d: &Digest,
    dest: &Path,
    mode: u32,
    rung: CowRung,
) -> Result<()> {
    let src = object_path(root, d);
    if !src.exists() {
        return Err(LightrError::NotFound(*d));
    }

    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }

    // Remove any stale dest so clonefile can succeed (it fails if dst exists).
    let _ = fs::remove_file(dest);

    cow_copy_file(&src, dest, rung)?;

    // Always apply the manifest mode (clonefile carries 0o444 from the store).
    set_mode(dest, mode)?;

    Ok(())
}

/// gc sweep only: chmod 0o644 then remove one object.
/// Object absent ⇒ Ok(()) (idempotent).
pub fn remove_object(root: &Path, d: &Digest) -> Result<()> {
    let path = object_path(root, d);
    if !path.exists() {
        return Ok(());
    }
    set_mode(&path, 0o644)?;
    fs::remove_file(&path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
