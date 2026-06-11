//! lightr-index — frozen contract: build-spec v2 §5 (ADR-0010).
//! Stat-index + walk + snapshot/hydrate/status ops. Bodies are WP-3.

use lightr_core::{Digest, Manifest, Result};
use lightr_store::{CowRung, Store};
use std::path::Path;

pub struct Index {
    _entries: Vec<u8>,
}

impl Index {
    pub fn load_for(_root: &Path) -> Result<Self> {
        todo!("WP-3: $LIGHTR_HOME/index/<blake3(root)>; empty if absent")
    }
    pub fn save_for(&self, _root: &Path) -> Result<()> {
        todo!("WP-3: atomic temp+rename")
    }
}

pub struct WalkReport {
    pub manifest: Manifest,
    pub rehashed: u64,
    pub from_index: u64,
}

pub fn scan(_root: &Path, _index: &mut Index) -> Result<WalkReport> {
    todo!("WP-3: parallel ignore-aware walk; stat-match → index digest; racily-clean rule")
}

pub struct SnapshotReport {
    pub root: Digest,
    pub files: u64,
    pub bytes_total: u64,
    pub objects_new: u64,
}

pub fn snapshot(_root: &Path, _store: &Store, _name: &str) -> Result<SnapshotReport> {
    todo!("WP-3: scan → ingest missing → manifest → ref_put (parent chain)")
}

pub struct HydrateReport {
    pub root: Digest,
    pub files: u64,
    pub bytes_total: u64,
    pub rung: CowRung,
}

pub fn hydrate(_dest: &Path, _store: &Store, _name: &str) -> Result<HydrateReport> {
    todo!("WP-3: ref → manifest → mkdirs + parallel materialize + symlinks; dest empty/absent")
}

pub struct StatusReport {
    pub clean: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

pub fn status(_root: &Path, _store: &Store, _name: &str) -> Result<StatusReport> {
    todo!("WP-3: scan vs ref manifest, path-sorted merge diff")
}
