//! lightr-store — frozen contract: build-spec v2 §4 (ADR-0009).
//! Object plane + refs + AC + CoW ladder. Bodies are WP-2.

use lightr_core::{Digest, RefRecord, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CowRung {
    Clone,
    Reflink,
    CopyRange,
    Copy,
}

pub struct Store {
    _root: PathBuf,
    _rung: CowRung,
}

impl Store {
    pub fn open(_root: impl Into<PathBuf>) -> Result<Self> {
        todo!("WP-2: create shards, probe CoW rung")
    }
    pub fn default_root() -> PathBuf {
        todo!("WP-2: $LIGHTR_HOME/store, default ~/.lightr")
    }
    pub fn rung(&self) -> CowRung {
        todo!("WP-2")
    }
    pub fn put_bytes(&self, _bytes: &[u8]) -> Result<Digest> {
        todo!("WP-2: atomic temp+rename, idempotent, chmod 0444")
    }
    pub fn ingest_file(&self, _path: &Path) -> Result<Digest> {
        todo!("WP-2: hash then CoW into store when rung allows")
    }
    pub fn get_bytes(&self, _d: &Digest) -> Result<Vec<u8>> {
        todo!("WP-2: rehash; mismatch=Integrity, evidence kept")
    }
    pub fn exists(&self, _d: &Digest) -> bool {
        todo!("WP-2")
    }
    pub fn materialize_file(&self, _d: &Digest, _dest: &Path, _mode: u32) -> Result<()> {
        todo!("WP-2: CoW ladder out + chmod")
    }
    pub fn ref_get(&self, _name: &str) -> Result<Option<RefRecord>> {
        todo!("WP-2")
    }
    pub fn ref_put(&self, _rec: &RefRecord) -> Result<()> {
        todo!("WP-2: last-write-wins, atomic")
    }
    pub fn ac_get(&self, _key: &Digest) -> Result<Option<Vec<u8>>> {
        todo!("WP-2")
    }
    pub fn ac_put(&self, _key: &Digest, _value: &[u8]) -> Result<()> {
        todo!("WP-2")
    }
}
