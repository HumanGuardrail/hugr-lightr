//! CAS object plane — put_bytes / ingest_file / get_bytes / materialize_file.
//!
//! Objects are content-addressed (blake3), sharded 2/62, stored read-only (0o444).
//! Writes go through a temp+rename+fsync pipeline for crash durability.

#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::fs::Permissions;
use std::fs::{self, File};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use lightr_core::{Digest, LightrError, Result};
use super::cow::{CowRung, try_cow_at_rung, cow_copy_file};
use super::lock::write_guard;

// ── path helpers ──────────────────────────────────────────────────────────────

/// Returns the two-char shard prefix and 62-char remainder from a Digest hex.
pub(super) fn shard_parts(hex: &str) -> (&str, &str) {
    (&hex[..2], &hex[2..])
}

/// Object path: <root>/objects/<2hex>/<62hex>
pub(super) fn object_path(root: &Path, d: &Digest) -> PathBuf {
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
pub fn materialize_file(root: &Path, d: &Digest, dest: &Path, mode: u32, rung: CowRung) -> Result<()> {
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

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::Store;

    fn tmp_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        (dir, store)
    }

    // ── object plane ────────────────────────────────────────────────────────

    #[test]
    fn roundtrip_put_get() {
        let (_dir, store) = tmp_store();
        let data = b"hello, lightr!";
        let d = store.put_bytes(data).unwrap();
        let got = store.get_bytes(&d).unwrap();
        assert_eq!(&got[..], data);
    }

    #[test]
    fn idempotent_double_put() {
        let (_dir, store) = tmp_store();
        let data = b"idempotent data";
        let d1 = store.put_bytes(data).unwrap();
        let d2 = store.put_bytes(data).unwrap();
        assert_eq!(d1, d2);
        // Verify get still works.
        assert_eq!(store.get_bytes(&d1).unwrap(), data);
    }

    #[test]
    fn integrity_corruption() {
        let (_dir, store) = tmp_store();
        let data = b"tamper me";
        let d = store.put_bytes(data).unwrap();

        // Locate the object file, relax permissions, flip a byte, restore.
        let obj_path = object_path(&store.root, &d);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&obj_path, Permissions::from_mode(0o644)).unwrap();
        }
        #[cfg(windows)]
        {
            let mut perms = fs::metadata(&obj_path).unwrap().permissions();
            perms.set_readonly(false);
            fs::set_permissions(&obj_path, perms).unwrap();
        }
        let mut bytes = fs::read(&obj_path).unwrap();
        bytes[0] ^= 0xFF;
        fs::write(&obj_path, &bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&obj_path, Permissions::from_mode(0o444)).unwrap();
        }
        #[cfg(windows)]
        {
            let mut perms = fs::metadata(&obj_path).unwrap().permissions();
            perms.set_readonly(true);
            fs::set_permissions(&obj_path, perms).unwrap();
        }

        let err = store.get_bytes(&d).unwrap_err();
        match err {
            LightrError::Integrity { expected, actual } => {
                assert_eq!(expected, d);
                assert_ne!(actual, d);
            }
            other => panic!("expected Integrity, got {:?}", other),
        }

        // Evidence file must still be present (never deleted).
        assert!(
            obj_path.exists(),
            "evidence file was deleted — violates spec"
        );
    }

    #[test]
    fn notfound() {
        let (_dir, store) = tmp_store();
        // Construct a digest that was never stored.
        let d = Digest::of_bytes(b"never stored");
        let err = store.get_bytes(&d).unwrap_err();
        assert!(matches!(err, LightrError::NotFound(_)));
    }

    // ── materialize ─────────────────────────────────────────────────────────

    #[test]
    fn materialize_preserves_bytes_and_mode() {
        let (dir, store) = tmp_store();
        let data = b"file content for materialize";
        let d = store.put_bytes(data).unwrap();

        let dest = dir.path().join("out").join("materialized.txt");
        store.materialize_file(&d, &dest, 0o755).unwrap();

        let got = fs::read(&dest).unwrap();
        assert_eq!(&got[..], data);

        // Mode check is unix-only (Windows uses ACLs, not mode bits).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(&dest).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "mode mismatch: got {mode:o}");
        }
    }

    #[test]
    fn materialize_notfound() {
        let (dir, store) = tmp_store();
        let d = Digest::of_bytes(b"not in store");
        let dest = dir.path().join("x");
        let err = store.materialize_file(&d, &dest, 0o644).unwrap_err();
        assert!(matches!(err, LightrError::NotFound(_)));
    }

    // ── ingest_file ──────────────────────────────────────────────────────────

    #[test]
    fn ingest_file_roundtrip() {
        let (dir, store) = tmp_store();
        let src = dir.path().join("input.txt");
        let data = b"ingest this file";
        fs::write(&src, data).unwrap();

        let d = store.ingest_file(&src).unwrap();

        // Object must be readable and correct.
        let got = store.get_bytes(&d).unwrap();
        assert_eq!(&got[..], data);
    }

    #[test]
    fn ingest_file_idempotent() {
        let (dir, store) = tmp_store();
        let src = dir.path().join("idem.txt");
        fs::write(&src, b"idempotent ingest").unwrap();

        let d1 = store.ingest_file(&src).unwrap();
        let d2 = store.ingest_file(&src).unwrap();
        assert_eq!(d1, d2);
    }

    // ── WP-A-dur: fsync path ─────────────────────────────────────────────────

    /// fsync path: write → drop store → reopen → content intact.
    /// fsync doesn't change observable behavior, but we assert the write+read
    /// roundtrip still works and the fsync helper is exercised.
    #[test]
    fn fsync_put_reopen_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("store");
        let data = b"durable object data";

        // Write and explicitly drop the store (simulates process restart).
        let digest = {
            let store = Store::open(&store_path).unwrap();
            store.put_bytes(data).unwrap()
        };

        // Reopen the store.
        let store2 = Store::open(&store_path).unwrap();
        let got = store2.get_bytes(&digest).unwrap();
        assert_eq!(&got[..], data);
    }

    /// Same fsync path via ingest_file.
    #[test]
    fn fsync_ingest_file_reopen_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("store");
        let src = dir.path().join("src.txt");
        let data = b"ingest durable data";
        fs::write(&src, data).unwrap();

        let digest = {
            let store = Store::open(&store_path).unwrap();
            store.ingest_file(&src).unwrap()
        };

        let store2 = Store::open(&store_path).unwrap();
        let got = store2.get_bytes(&digest).unwrap();
        assert_eq!(&got[..], data);
    }

    // ── R1: remove_object ────────────────────────────────────────────────────

    #[test]
    fn remove_object_removes_and_is_idempotent() {
        let (_dir, store) = tmp_store();
        let data = b"to be removed";
        let d = store.put_bytes(data).unwrap();

        assert!(store.exists(&d), "object must exist after put");
        store.remove_object(&d).unwrap();
        assert!(!store.exists(&d), "object must not exist after remove");

        // Second remove must be Ok(()) — idempotent.
        store.remove_object(&d).unwrap();
    }

    #[test]
    fn remove_object_absent_is_ok() {
        let (_dir, store) = tmp_store();
        let d = Digest::of_bytes(b"never stored");
        // Must succeed without error.
        store.remove_object(&d).unwrap();
    }
}
