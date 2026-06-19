//! NFS-loopback view backend for macOS (the EdenFS-proven route).
//!
//! Planned ADR-0013 S3 spike: an in-process NFSv3 server bound to loopback
//! answers the kernel NFS client mounted at the view root; reads fault content
//! from the store (ADR-0013 §2). FSKit is adopted later when S1/S3-class
//! testing says it's ready. **Intentionally not yet wired into the run path**
//! — the shipped materialization is CoW hydrate via `lightr_index`. This
//! module is the planned implementation; every method returns
//! `ErrorKind::Unsupported` until the S3 spike validates it on a macOS box.

use crate::{ViewBackend, ViewPlan};
use std::path::Path;

/// In-process NFS-loopback server for the planned ADR-0013 S3 spike.
/// Holds the bound socket, the served plan, and the mount point once the
/// spike implementation lands.
#[derive(Debug, Default)]
pub struct NfsLoopbackBackend {
    // ADR-0013: real fields (listening socket, export table, mount point,
    // server task handle) land with the S3 spike implementation.
    _seam: (),
}

impl NfsLoopbackBackend {
    /// Construct an unmounted, unstarted NFS-loopback backend.
    pub fn new() -> Self {
        Self::default()
    }
}

/// ADR-0013 S3 spike: bind the in-process NFSv3 server to loopback and
/// start serving `plan`. Real socket/server code; intentionally not yet
/// implemented. Returns `Unsupported` until the spike lands.
// ADR-0013
fn start_nfs_server(_plan: &ViewPlan) -> std::io::Result<()> {
    // ADR-0013 S3: bind 127.0.0.1, register the export, spawn the RPC
    // loop that faults content from the store. Lands with the S3 spike.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "nfsloopback O(1) view backend is a planned spike (ADR-0013); \
         the shipped runtime materializes via CoW hydrate",
    ))
}

impl ViewBackend for NfsLoopbackBackend {
    // ADR-0013: mount is intentionally not yet wired into the run path.
    fn mount(&mut self, plan: &ViewPlan, at: &Path) -> std::io::Result<()> {
        // ADR-0013 S3: start the loopback NFS server, then mount(2) the
        // kernel NFS client at `at`. Localized unsafe lands here when the
        // spike is validated on a macOS box.
        let _ = at; // future mount point for the kernel NFS client
        let _ = start_nfs_server(plan);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "nfsloopback O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }

    // ADR-0013: fault_in is intentionally not yet wired into the run path.
    fn fault_in(&mut self, _path: &str) -> std::io::Result<()> {
        // ADR-0013 S3: server-side readahead — pre-stage one entry's
        // content from the store so the next NFS READ is warm. Lands with
        // the S3 spike.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "nfsloopback O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }

    // ADR-0013: unmount is intentionally not yet wired into the run path.
    fn unmount(&mut self, _at: &Path) -> std::io::Result<()> {
        // ADR-0013 S3: unmount(2) the NFS client and stop the server once
        // the solidifier reports fully-solid. Lands with the S3 spike.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "nfsloopback O(1) view backend is a planned spike (ADR-0013); \
             the shipped runtime materializes via CoW hydrate",
        ))
    }
}
