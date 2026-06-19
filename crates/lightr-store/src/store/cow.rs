//! CoW ladder — platform-gated copy-on-write strategies.
//!
//! Probe finds the best available rung; `cow_copy_file` applies it with
//! a silent-correct fallback to `std::fs::copy`.

#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

// ─────────────────────────────── CowRung ────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CowRung {
    Clone,
    Reflink,
    CopyRange,
    /// Windows: best-effort ReFS block-clone via FSCTL_DUPLICATE_EXTENTS_TO_FILE.
    /// Falls through to Copy on NTFS or any failure. // WIN-PATH
    RefsBlockClone,
    Copy,
}

// ─────────────────────────── CoW ladder (platform-gated) ────────────────────

/// Probe: find the best CoW rung available under `root`.
/// Creates <root>/.probe-src, tries the ladder toward <root>/.probe-dst, cleans up both.
pub fn probe_rung(root: &Path) -> CowRung {
    let src = root.join(".probe-src");
    let dst = root.join(".probe-dst");

    // Write a tiny probe source file.
    if File::create(&src)
        .and_then(|mut f| f.write_all(b"probe"))
        .is_err()
    {
        return CowRung::Copy;
    }

    let rung = try_ladder_probe(&src, &dst);

    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);

    rung
}

fn try_ladder_probe(src: &Path, dst: &Path) -> CowRung {
    #[cfg(target_os = "macos")]
    {
        let _ = fs::remove_file(dst);
        if cow_clone(src, dst).is_ok() {
            return CowRung::Clone;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = fs::remove_file(dst);
        if cow_reflink(src, dst).is_ok() {
            return CowRung::Reflink;
        }
        let _ = fs::remove_file(dst);
        if cow_copy_range(src, dst).is_ok() {
            return CowRung::CopyRange;
        }
    }

    // WIN-PATH: probe for ReFS block-clone capability on Windows.
    #[cfg(windows)]
    {
        let _ = fs::remove_file(dst);
        if cow_refs_block_clone(src, dst).is_ok() {
            return CowRung::RefsBlockClone;
        }
    }

    CowRung::Copy
}

/// macOS: libc::clonefile(src, dst, 0)
#[cfg(target_os = "macos")]
fn cow_clone(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::ffi::CString;

    let src_c = CString::new(src.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let dst_c = CString::new(dst.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let ret = unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Linux: ioctl FICLONE (0x40049409)
#[cfg(target_os = "linux")]
fn cow_reflink(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let src_file = File::open(src)?;
    let dst_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;
    const FICLONE: libc::c_ulong = 0x40049409;
    let ret = unsafe { libc::ioctl(dst_file.as_raw_fd(), FICLONE, src_file.as_raw_fd()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Linux: copy_file_range loop
#[cfg(target_os = "linux")]
fn cow_copy_range(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let src_file = File::open(src)?;
    let dst_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;
    let src_len = src_file.metadata()?.len();
    let mut off_in: libc::loff_t = 0;
    let mut off_out: libc::loff_t = 0;
    let mut remaining = src_len as usize;

    while remaining > 0 {
        let n = unsafe {
            libc::copy_file_range(
                src_file.as_raw_fd(),
                &mut off_in as *mut libc::loff_t,
                dst_file.as_raw_fd(),
                &mut off_out as *mut libc::loff_t,
                remaining,
                0,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            break;
        }
        remaining -= n as usize;
    }
    Ok(())
}

/// Windows: best-effort ReFS block-clone via FSCTL_DUPLICATE_EXTENTS_TO_FILE.
///
/// Only succeeds on ReFS volumes that support block-clone (DUPLICATE_EXTENTS_DATA).
/// Falls through (returns Err) on NTFS or any failure — the caller then uses
/// std::fs::copy as the required-correct fallback path.
///
/// // WIN-PATH — this path is only exercisable on a ReFS volume on a real Windows box.
#[cfg(windows)]
fn cow_refs_block_clone(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::FALSE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandleEx, FILE_STANDARD_INFO,
    };
    use windows_sys::Win32::System::Ioctl::{
        DUPLICATE_EXTENTS_DATA, FSCTL_DUPLICATE_EXTENTS_TO_FILE,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    // Open src for reading.
    let src_file = File::open(src)?;
    let src_handle = src_file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;

    // Get file size.
    let mut std_info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            src_handle,
            FileStandardInfo,
            &mut std_info as *mut _ as *mut _,
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if ok == FALSE {
        return Err(std::io::Error::last_os_error());
    }
    let file_size = std_info.EndOfFile;

    // Create / truncate dst, then PRE-SIZE it: FSCTL_DUPLICATE_EXTENTS_TO_FILE
    // requires the destination region to already exist — a length-0 dst is
    // rejected, which is why this fast path never engaged before.
    let dst_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;
    dst_file.set_len(file_size as u64)?;
    let dst_handle = dst_file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;

    // Build DUPLICATE_EXTENTS_DATA — clone the whole file from offset 0.
    // NOTE: the FSCTL also wants ByteCount cluster-aligned; modern ReFS accepts a
    // non-aligned final (EOF) extent, older volumes do not. This path is
    // best-effort + WIN-PATH (unvalidated here): any rejection returns Err and
    // the caller falls back to std::fs::copy (the required-correct path).
    let dup_data = DUPLICATE_EXTENTS_DATA {
        FileHandle: src_handle,
        SourceFileOffset: 0,
        TargetFileOffset: 0,
        ByteCount: file_size,
    };

    let mut bytes_returned: u32 = 0;
    let result = unsafe {
        DeviceIoControl(
            dst_handle,
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            &dup_data as *const _ as *const _,
            std::mem::size_of::<DUPLICATE_EXTENTS_DATA>() as u32,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    if result == FALSE {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Apply the CoW ladder to copy src→dst at the given rung; falls back to
/// std::fs::copy on failure (silent-correct, not silent-hidden — counted
/// at the call sites but not exposed in the API per R0 law).
pub fn cow_copy_file(src: &Path, dst: &Path, rung: CowRung) -> lightr_core::Result<()> {
    // Ensure parent exists.
    if let Some(p) = dst.parent() {
        fs::create_dir_all(p)?;
    }

    match try_cow_at_rung(src, dst, rung) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Fall through to std::fs::copy (silently-correct per spec).
            let _ = fs::remove_file(dst); // remove partial if any
            fs::copy(src, dst)?;
            Ok(())
        }
    }
}

pub fn try_cow_at_rung(src: &Path, dst: &Path, rung: CowRung) -> std::io::Result<()> {
    match rung {
        #[cfg(target_os = "macos")]
        CowRung::Clone => cow_clone(src, dst),
        #[cfg(target_os = "linux")]
        CowRung::Reflink => cow_reflink(src, dst),
        #[cfg(target_os = "linux")]
        CowRung::CopyRange => cow_copy_range(src, dst),
        // WIN-PATH: attempt ReFS block-clone; falls through to Copy on failure.
        #[cfg(windows)]
        CowRung::RefsBlockClone => cow_refs_block_clone(src, dst),
        // CowRung::Copy (always available) or any rung on the wrong platform:
        _ => {
            fs::copy(src, dst)?;
            Ok(())
        }
    }
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

    // ── rung ─────────────────────────────────────────────────────────────────

    #[test]
    fn rung_returns_probed_value() {
        let (_dir, store) = tmp_store();
        // Just assert it's a valid CowRung variant — the value is machine-dependent.
        let r = store.rung();
        let valid = matches!(
            r,
            CowRung::Clone
                | CowRung::Reflink
                | CowRung::CopyRange
                | CowRung::RefsBlockClone
                | CowRung::Copy
        );
        assert!(valid);
    }
}
