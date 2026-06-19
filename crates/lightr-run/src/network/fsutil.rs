//! Filesystem utilities: flock RAII guard, atomic write, fsync dir.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

// ───────────────────────── flock guard (RAII) ──────────────────────────────

/// RAII advisory-lock guard over a network's `.lock` file. Mirrors
/// `lightr-store`'s `WriteGuard`/`GcGuard`: the held `File` keeps the lock; the
/// `Drop` releases it (closing the fd would also release, we call `LOCK_UN`
/// explicitly for clarity).
pub(super) struct FlockGuard {
    _file: File,
}

impl FlockGuard {
    /// Acquire an advisory lock (`LOCK_SH` or `LOCK_EX`) on `lock_path`.
    pub(super) fn acquire(lock_path: &Path, exclusive: bool) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        let op = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        let ret = unsafe { libc::flock(file.as_raw_fd(), op) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(FlockGuard { _file: file })
    }
}

impl Drop for FlockGuard {
    fn drop(&mut self) {
        let fd = self._file.as_raw_fd();
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
    }
}

// ─────────────────────────── fs helpers ────────────────────────────────────

/// fsync the parent directory so a rename (directory-entry change) is durable.
pub(super) fn fsync_dir(dir: &Path) -> io::Result<()> {
    let f = File::open(dir)?;
    f.sync_all()?;
    Ok(())
}

/// Atomic write: temp file in `parent`, fsync the file, rename to `dest`, then
/// fsync `parent` so the rename is crash-durable (mirrors lightr-store).
pub(super) fn atomic_write(parent: &Path, dest: &Path, data: &[u8]) -> io::Result<()> {
    fs::create_dir_all(parent)?;
    let nonce = format!(
        ".tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = parent.join(nonce);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dest)?;
    fsync_dir(parent)?;
    Ok(())
}
