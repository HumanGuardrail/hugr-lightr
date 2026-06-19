//! ProjFS / ReFS-overlay view backend for Windows.
//!
//! Planned ADR-0013 S1/S3 spike: on Windows, views are surfaced via the
//! Windows Projected File System (ProjFS) API, which allows a provider process
//! to present a virtual directory tree whose content is faulted in on demand
//! from the CAS store.  A ReFS block-clone
//! (FSCTL_DUPLICATE_EXTENTS_TO_FILE) backs solidification on ReFS volumes;
//! NTFS falls back to a copy.  Neither ProjFS nor ReFS is available inside
//! WSL2 from the guest side, so the Windows isolation model runs the engine
//! natively and the view provider runs as a user-mode Windows process.
//!
//! **Intentionally not yet wired into the run path** — the shipped
//! materialization is CoW hydrate via `lightr_index`. Every method returns
//! `ErrorKind::Unsupported`; the real ProjFS/FFI work lands with the ADR-0013
//! S1/S3 spike on a Windows target box.

use crate::{ViewBackend, ViewPlan};
use std::path::Path;

/// ProjFS-backed view for Windows.  Holds the virtualization root handle and
/// the active plan once the ADR-0013 S1/S3 spike implementation lands.
///
/// **ADR-0013:** intentionally not yet wired into the run path — no ProjFS
/// handle is opened until the spike validates this path on a Windows box.
#[derive(Debug, Default)]
pub struct ProjFsBackend {
    // ADR-0013: real fields (HVIRTUAL_STORAGE_VIRTUAL_DISK handle,
    // notification callbacks, async task handle) land with the S1/S3 spike.
    _seam: (),
}

impl ProjFsBackend {
    /// Construct an unmounted ProjFS backend.
    pub fn new() -> Self {
        Self::default()
    }
}

/// ADR-0013 S1/S3 spike: initialise the ProjFS virtualization root at
/// `root` and register the provider callbacks that fault content from the
/// store on demand (PrjStartVirtualizing / callback registration).
/// Intentionally not yet implemented; returns `Unsupported` until the
/// spike is validated on a Windows box.
// ADR-0013
fn start_projfs_provider(_plan: &ViewPlan, _root: &Path) -> std::io::Result<()> {
    // ADR-0013 S1: call PrjMarkDirectoryAsPlaceholder, then
    // PrjStartVirtualizing with GET_FILE_DATA / NOTIFY callbacks that read
    // chunk data from the lightr-store CAS. Lands with the S1/S3 spike.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "projfs O(1) view backend is a planned spike (ADR-0013); \
         the shipped runtime materializes via CoW hydrate",
    ))
}

/// ADR-0013 S1/S3 spike: stop the ProjFS virtualization root and release
/// the provider handle (PrjStopVirtualizing). Intentionally not yet
/// implemented; returns `Unsupported` until the spike lands.
// ADR-0013
fn stop_projfs_provider() -> std::io::Result<()> {
    // ADR-0013 S1: call PrjStopVirtualizing on the stored handle.
    // Lands with the S1/S3 spike.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "projfs O(1) view backend is a planned spike (ADR-0013); \
         the shipped runtime materializes via CoW hydrate",
    ))
}

impl ViewBackend for ProjFsBackend {
    // ADR-0013: mount is intentionally not yet wired into the run path.
    fn mount(&mut self, plan: &ViewPlan, at: &Path) -> std::io::Result<()> {
        // ADR-0013 S1: mark `at` as a ProjFS placeholder root and start
        // the provider (PrjMarkDirectoryAsPlaceholder +
        // PrjStartVirtualizing). Lands with the S1/S3 spike on Windows.
        let _ = start_projfs_provider(plan, at);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "projfs O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }

    // ADR-0013: fault_in is intentionally not yet wired into the run path.
    fn fault_in(&mut self, _path: &str) -> std::io::Result<()> {
        // ADR-0013 S1: convert the placeholder at `path` to a hydrated
        // file via PrjWriteFileData — the GET_FILE_DATA callback path,
        // called lazily by the kernel. Lands with the S1/S3 spike.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "projfs O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }

    // ADR-0013: unmount is intentionally not yet wired into the run path.
    fn unmount(&mut self, _at: &Path) -> std::io::Result<()> {
        // ADR-0013 S1: stop the ProjFS provider once the solidifier reports
        // fully-solid (every file is a real on-disk CoW clone).
        // PrjStopVirtualizing. Lands with the S1/S3 spike on Windows.
        let _ = stop_projfs_provider();
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "projfs O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }
}
