//! lightr-oci — frozen contract: build-spec-r2.md §3 (bodies: WP R2-W1).
//! BRIDGE crate: the only place network code may live (ADR-0011).

use lightr_core::{Digest, Result};
use lightr_store::Store;
use std::path::Path;

pub struct ImportReport {
    pub name: String,
    pub root: Digest,
    pub layers: u64,
    pub files: u64,
}

/// Import an OCI layout directory or tar — pure local, no network.
pub fn import_layout(_path: &Path, _store: &Store, _name: &str) -> Result<ImportReport> {
    todo!("R2-W1")
}

/// Pull from an OCI registry (anonymous + docker.io token dance), then import.
pub fn pull(_image: &str, _store: &Store, _name: &str) -> Result<ImportReport> {
    todo!("R2-W1")
}
