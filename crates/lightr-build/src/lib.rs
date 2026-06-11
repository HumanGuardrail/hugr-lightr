//! lightr-build — frozen contract: build-spec-r3.md §2+§3.
//! Dockerfile build graph (step-memoized) + lazy compose. Bodies: R3-W1/W2.

use lightr_core::{Digest, Result};
use lightr_store::Store;
use std::path::Path;

// ── §2 Dockerfile build ─────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instr {
    From { image_ref: String },
    Run { argv: Vec<String> },
    Copy { src: Vec<String>, dest: String },
    Env { key: String, val: String },
    Workdir { path: String },
    Cmd { argv: Vec<String> },
    Label { key: String, val: String },
}

#[derive(Clone, Debug)]
pub struct BuildStep {
    pub instr: Instr,
    pub raw: String,
}

pub fn parse_dockerfile(_text: &str) -> Result<Vec<BuildStep>> {
    todo!("R3-W1")
}

pub struct BuildReport {
    pub name: String,
    pub root: Digest,
    pub steps: u64,
    pub cached_steps: u64,
}

pub fn build(
    _context_dir: &Path,
    _dockerfile: &Path,
    _name: &str,
    _engine: lightr_engine::EngineKind,
    _store: &Store,
) -> Result<BuildReport> {
    todo!("R3-W1")
}

// ── §3 lazy compose ──────────────────────────────────────────────────────────

pub struct Service {
    pub name: String,
    pub image_ref: String,
    pub command: Option<Vec<String>>,
    pub ports: Vec<(u16, u16)>,
    pub env: Vec<(String, String)>,
    pub eager: bool,
}

pub struct Compose {
    pub services: Vec<Service>,
}

pub fn parse_compose(_yaml: &str) -> Result<Compose> {
    todo!("R3-W2")
}

pub struct ComposeHandle {
    pub stack_dir: std::path::PathBuf,
    pub services: Vec<String>,
}

pub fn compose_up(_c: &Compose, _store: &Store, _ttl_secs: u64) -> Result<ComposeHandle> {
    todo!("R3-W2")
}

pub fn compose_down(_stack_dir: &Path) -> Result<()> {
    todo!("R3-W2")
}
