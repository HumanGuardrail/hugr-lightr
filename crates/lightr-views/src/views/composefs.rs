//! composefs / EROFS view backend for Linux (kernel-native, no FUSE).
//!
//! Planned ADR-0013 S1/S3 spike: EROFS metadata image generated over the
//! store's objects, mounted with an overlay upper layer for writes; pairs with
//! fs-verity (ADR-0013 §1). **Intentionally not yet wired into the run path**
//! — the shipped materialization is CoW hydrate via `lightr_index`. This
//! module is the planned implementation; every method returns
//! `ErrorKind::Unsupported` until the S1/S3 spike validates it on a Linux
//! target box.

use crate::{ViewBackend, ViewPlan};
use std::path::Path;

/// composefs/EROFS-backed view. Holds whatever the kernel mount needs
/// (loop device, EROFS image path, overlay dirs) once the ADR-0013 S1/S3
/// spike is implemented.
#[derive(Debug, Default)]
pub struct ComposefsBackend {
    // ADR-0013: real fields (EROFS image path, loop fd, overlay upper/work
    // dirs) land with the S1/S3 spike implementation.
    _seam: (),
}

impl ComposefsBackend {
    /// Construct an unmounted composefs backend.
    pub fn new() -> Self {
        Self::default()
    }
}

/// ADR-0013 S1 spike: build the EROFS metadata image describing `plan`
/// over the store's objects (mkfs.erofs / image-layout). Intentionally not
/// yet implemented; returns `Unsupported` until the spike lands.
// ADR-0013
fn build_erofs_image(_plan: &ViewPlan, _image_out: &Path) -> std::io::Result<()> {
    // ADR-0013 S1: composefs O(1) view backend is a planned spike; the
    // shipped runtime materializes via CoW hydrate (lightr_index).
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "composefs O(1) view backend is a planned spike (ADR-0013); \
         the shipped runtime materializes via CoW hydrate",
    ))
}

impl ViewBackend for ComposefsBackend {
    // ADR-0013: mount is intentionally not yet wired into the run path.
    fn mount(&mut self, plan: &ViewPlan, at: &Path) -> std::io::Result<()> {
        // ADR-0013 S1: build EROFS image, loop-mount it, stack an overlay
        // upper for writes. Real kernel syscalls (mount(2)) — localized
        // unsafe lands here when the spike is validated on a Linux box.
        let _ = build_erofs_image(plan, at);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "composefs O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }

    // ADR-0013: fault_in is intentionally not yet wired into the run path.
    fn fault_in(&mut self, _path: &str) -> std::io::Result<()> {
        // ADR-0013 S1: EROFS faults pages from the store via the kernel;
        // this hook is for predictive readahead. Lands with the S1 spike.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "composefs O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }

    // ADR-0013: unmount is intentionally not yet wired into the run path.
    fn unmount(&mut self, _at: &Path) -> std::io::Result<()> {
        // ADR-0013 S1: umount(2) the overlay + loop device once the
        // solidifier reports fully-solid. Lands with the S1 spike.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "composefs O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }
}
