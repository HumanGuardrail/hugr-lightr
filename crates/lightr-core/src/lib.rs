//! lightr-core — frozen contract: build-spec v2 §3 (ADR-0009/0004).
//! Types are the contract; method bodies are WP-1.
#![forbid(unsafe_code)]

use std::path::Path;

pub const OUTPUT_CAP_BYTES: usize = 5 * 1024 * 1024;
pub const MANIFEST_MAGIC: &[u8; 4] = b"LMF1";
pub const REF_KEY_DOMAIN: &str = "lightr/ref/v1/";

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    pub fn of_bytes(_data: &[u8]) -> Self {
        todo!("WP-1")
    }
    pub fn of_file(_path: &Path) -> Result<Self> {
        todo!("WP-1")
    }
    pub fn to_hex(&self) -> String {
        todo!("WP-1")
    }
    pub fn from_hex(_s: &str) -> Result<Self> {
        todo!("WP-1")
    }
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("WP-1")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    File { path: String, mode: u32, size: u64, digest: Digest },
    Symlink { path: String, target: String },
    Dir { path: String },
}

impl Entry {
    pub fn path(&self) -> &str {
        match self {
            Entry::File { path, .. } | Entry::Symlink { path, .. } | Entry::Dir { path } => path,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub total_size: u64,
    pub entries: Vec<Entry>,
}

impl Manifest {
    pub fn encode(&self) -> Vec<u8> {
        todo!("WP-1: LMF1 codec, build-spec v2 §3")
    }
    pub fn decode(_bytes: &[u8]) -> Result<Self> {
        todo!("WP-1")
    }
    pub fn digest(&self) -> Digest {
        todo!("WP-1")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefRecord {
    pub name: String,
    pub root: Digest,
    pub parent: Option<Digest>,
    pub created_at_unix: u64,
    pub tool_version: String,
}

impl RefRecord {
    pub fn encode(&self) -> Vec<u8> {
        todo!("WP-1")
    }
    pub fn decode(_bytes: &[u8]) -> Result<Self> {
        todo!("WP-1")
    }
}

pub fn validate_ref_name(_name: &str) -> Result<()> {
    todo!("WP-1: ADR-0004 grammar ^(@[a-z0-9-]+/)?[a-z0-9._-]{{1,64}}$")
}

pub fn ref_key(_name: &str) -> Digest {
    todo!("WP-1: BLAKE3(REF_KEY_DOMAIN || name)")
}

#[derive(Debug)]
pub enum LightrError {
    NotFound(Digest),
    RefNotFound(String),
    Integrity { expected: Digest, actual: Digest },
    TooLarge { size: u64, cap: u64 },
    InvalidRef(String),
    InvalidManifest(String),
    Io(std::io::Error),
}

impl std::fmt::Display for LightrError {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("WP-1")
    }
}

impl std::error::Error for LightrError {}

impl From<std::io::Error> for LightrError {
    fn from(e: std::io::Error) -> Self {
        LightrError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, LightrError>;
