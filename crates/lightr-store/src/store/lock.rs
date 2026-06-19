//! Advisory flock guards — WriteGuard (SHARED) and GcGuard (EXCLUSIVE).
//!
//! Ordering: writers take LOCK_SH; gc takes LOCK_EX on `<root>/.gc.lock`.
//! gc cannot sweep an object that a concurrent writer is mid-publishing —
//! the exclusive lock blocks until all shared locks drop.

use lightr_core::{LightrError, Result};
use std::fs::File;
use std::path::Path;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

// ─────────────────────────────── gc lock guards ──────────────────────────────

/// Held by a writer (put_bytes, ingest_file, ref_put, ac_put) for the duration
/// of its write.  Acquires a SHARED advisory flock on `<root>/.gc.lock`.
///
/// Ordering: writers take LOCK_SH; gc takes LOCK_EX on the same file.
/// This means gc cannot sweep an object that a concurrent writer is
/// mid-publishing — the exclusive lock blocks until all shared locks drop.
pub struct WriteGuard {
    pub(super) _file: File,
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        // LOCK_UN is released automatically when the File fd closes, but
        // we call it explicitly for clarity and portability.
        #[cfg(unix)]
        {
            let fd = self._file.as_raw_fd();
            unsafe {
                libc::flock(fd, libc::LOCK_UN);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
            use windows_sys::Win32::System::IO::OVERLAPPED;
            let handle = self._file.as_raw_handle();
            if handle != INVALID_HANDLE_VALUE as _ {
                let mut ol: OVERLAPPED = unsafe { std::mem::zeroed() };
                unsafe { UnlockFileEx(handle as _, 0, u32::MAX, u32::MAX, &mut ol) };
            }
        }
    }
}

/// Held by gc for the entire mark+sweep pass.  Acquires an EXCLUSIVE advisory
/// flock on `<root>/.gc.lock`, blocking until all in-flight writers have
/// released their shared locks.
pub struct GcGuard {
    pub(super) _file: File,
}

impl Drop for GcGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let fd = self._file.as_raw_fd();
            unsafe {
                libc::flock(fd, libc::LOCK_UN);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
            use windows_sys::Win32::System::IO::OVERLAPPED;
            let handle = self._file.as_raw_handle();
            if handle != INVALID_HANDLE_VALUE as _ {
                let mut ol: OVERLAPPED = unsafe { std::mem::zeroed() };
                unsafe { UnlockFileEx(handle as _, 0, u32::MAX, u32::MAX, &mut ol) };
            }
        }
    }
}

// ── Store lock helpers ────────────────────────────────────────────────────────

/// Returns the path to the gc advisory lock file.
pub(super) fn gc_lock_path(root: &Path) -> std::path::PathBuf {
    root.join(".gc.lock")
}

/// Open (create if absent) the gc lock file.
pub(super) fn gc_lock_file(root: &Path) -> Result<File> {
    let path = gc_lock_path(root);
    // create + read + write + truncate(false): open for locking only;
    // we never write content, so we explicitly preserve any existing bytes.
    let f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    Ok(f)
}

/// Acquire a SHARED advisory lock on `<root>/.gc.lock`.
///
/// Writers (put_bytes, ingest_file, ref_put, ac_put) hold this for the
/// duration of their write.  Multiple writers may proceed concurrently.
/// gc's exclusive lock cannot be granted while any shared lock is held,
/// so gc cannot sweep an object that a concurrent writer is mid-publishing.
pub fn write_guard(root: &Path) -> Result<WriteGuard> {
    let f = gc_lock_file(root)?;
    #[cfg(unix)]
    {
        let fd = f.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_SH) };
        if ret != 0 {
            return Err(LightrError::Io(std::io::Error::last_os_error()));
        }
    }
    #[cfg(windows)]
    {
        // WIN-PATH: shared (non-exclusive) lock via LockFileEx.
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::FALSE;
        use windows_sys::Win32::Storage::FileSystem::LockFileEx;
        use windows_sys::Win32::System::IO::OVERLAPPED;
        // Flags = 0: shared, blocking.
        let mut ol: OVERLAPPED = unsafe { std::mem::zeroed() };
        let ret =
            unsafe { LockFileEx(f.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX, &mut ol) };
        if ret == FALSE {
            return Err(LightrError::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(WriteGuard { _file: f })
}

/// Acquire an EXCLUSIVE advisory lock on `<root>/.gc.lock`.
///
/// Held by gc for the full mark+sweep pass.  Blocks until all in-flight
/// writer shared locks have been released.
pub fn gc_guard(root: &Path) -> Result<GcGuard> {
    let f = gc_lock_file(root)?;
    #[cfg(unix)]
    {
        let fd = f.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if ret != 0 {
            return Err(LightrError::Io(std::io::Error::last_os_error()));
        }
    }
    #[cfg(windows)]
    {
        // WIN-PATH: exclusive lock via LockFileEx with LOCKFILE_EXCLUSIVE_LOCK.
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::FALSE;
        use windows_sys::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK};
        use windows_sys::Win32::System::IO::OVERLAPPED;
        let mut ol: OVERLAPPED = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            LockFileEx(
                f.as_raw_handle() as _,
                LOCKFILE_EXCLUSIVE_LOCK,
                0,
                u32::MAX,
                u32::MAX,
                &mut ol,
            )
        };
        if ret == FALSE {
            return Err(LightrError::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(GcGuard { _file: f })
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use crate::Store;

    fn tmp_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        (dir, store)
    }

    // ── WP-A-dur: write_guard / gc_guard basic ───────────────────────────────

    #[test]
    fn write_guard_acquired_and_released() {
        let (_dir, store) = tmp_store();
        let wg = store.write_guard().unwrap();
        // Multiple shared locks can coexist.
        let wg2 = store.write_guard().unwrap();
        drop(wg);
        drop(wg2);
    }

    #[test]
    fn gc_guard_acquired_and_released() {
        let (_dir, store) = tmp_store();
        let gg = store.gc_guard().unwrap();
        drop(gg);
        // After release, a new gc_guard must succeed.
        let gg2 = store.gc_guard().unwrap();
        drop(gg2);
    }

    // ── WP-A-dur: gc-vs-writer lock test ────────────────────────────────────

    /// gc/writer lock contract: an EXCLUSIVE `gc_guard` must BLOCK while any
    /// SHARED `write_guard` is held, and only proceed once it's released.
    ///
    /// This is the real guarantee the flock gives — gc cannot run its
    /// mark+sweep concurrently with an in-flight write (so it never sees a
    /// torn/partial object or races a rename). It is NOT a claim that an
    /// unreferenced `put_bytes` survives gc (an object with no ref IS garbage
    /// and real gc rightly sweeps it).
    ///
    /// Deterministic: a holder thread takes `write_guard` and sleeps; the main
    /// thread times `gc_guard()` acquisition. If the lock were a no-op the
    /// exclusive guard would return immediately and the elapsed-time assertion
    /// would fire.
    #[test]
    fn gc_guard_blocks_until_write_guard_released() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};

        let (_dir, store) = tmp_store();
        let store = std::sync::Arc::new(store);

        const HOLD: Duration = Duration::from_millis(300);
        let (tx, rx) = mpsc::channel::<()>();

        // Holder: take the SHARED write guard, signal, hold it for HOLD, release.
        let store_h = std::sync::Arc::clone(&store);
        let holder = thread::spawn(move || {
            let _wg = store_h.write_guard().unwrap();
            tx.send(()).unwrap(); // signal "shared lock is held"
            thread::sleep(HOLD);
            // _wg dropped here → LOCK_UN
        });

        // Wait until the shared lock is definitely held, then time the
        // exclusive acquisition — it must block ~HOLD until the holder releases.
        rx.recv().unwrap();
        let start = Instant::now();
        let gg = store.gc_guard().unwrap();
        let waited = start.elapsed();
        drop(gg);
        holder.join().unwrap();

        // Allow generous slack for scheduling, but it MUST have blocked a
        // substantial fraction of HOLD — a no-op lock returns in ~microseconds.
        assert!(
            waited >= HOLD / 2,
            "gc_guard must block while a write_guard is held: waited only {waited:?} \
             (expected ≥ {:?}) — the shared/exclusive flock protocol is broken",
            HOLD / 2
        );
    }
}
