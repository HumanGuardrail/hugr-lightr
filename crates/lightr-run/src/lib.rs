//! lightr-run — frozen contract: build-spec v2 §6.
//! Memo key, native exec, replay. Bodies are WP-4.

use lightr_core::{Digest, Result};
use lightr_store::Store;
use std::path::PathBuf;

pub struct RunSpec {
    pub cwd: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub command: Vec<String>,
    pub env_keys: Vec<String>,
}

pub struct RunOutcome {
    pub key: Digest,
    pub hit: bool,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn run_memoized(_spec: &RunSpec, _store: &Store) -> Result<RunOutcome> {
    todo!("WP-4: key=BLAKE3(domain‖input manifests‖argv‖env‖triple); exit-0-only memo; cap; replay")
}
