//! CRI seam vocabulary — TRANSCRIBED from lightr-cri @ seam-contract-v1.1.
//!
//! Provenance: these shapes are EXACT copies of the frozen seam types defined
//! in `lightr-cri/crates/lightr-cri-backend/src/{lib.rs,vocab.rs}` and the
//! semantics in `lightr-cri/docs/contract/seam-contract-v1.1.md` (FROZEN
//! 2026-06-12). They are TRANSCRIBED, NOT a git/path dependency — the
//! wire-level seam is proven later by the shared conformance vectors
//! (WP-CRI-VECTORS), never by a crate import. Drift is caught by those
//! vectors, not by the compiler. Do not "improve" these shapes here.

use std::collections::BTreeMap;

// ── §A Vocabulary additions (v1.1) ───────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DnsConfig {
    pub servers: Vec<String>,
    pub searches: Vec<String>,
    pub options: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Protocol {
    Tcp,
    Udp,
    Sctp,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortMapping {
    pub protocol: Protocol,
    pub container_port: i32,
    pub host_port: i32,
    #[serde(default)]
    pub host_ip: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthConfig {
    pub username: String,
    pub password: String,
    pub auth: String,
    pub server_address: String,
}

// ── Identifiers ──────────────────────────────────────────────────────────────

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct SandboxId(pub String);

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ContainerId(pub String);

// ── Sandbox plane ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxConfig {
    pub name: String,
    pub uid: String,
    pub namespace: String,
    pub attempt: u32,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub log_directory: String,
    // v1.1 additions (all serde(default) — old state files load unchanged)
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub host_network: bool,
    #[serde(default)]
    pub dns: Option<DnsConfig>,
    #[serde(default)]
    pub port_mappings: Vec<PortMapping>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SandboxState {
    Ready,
    NotReady,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxStatus {
    pub id: SandboxId,
    pub config: SandboxConfig,
    pub state: SandboxState,
    pub created_at_nanos: i64,
    // v1.1 additions
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub netns_path: Option<String>,
}

// ── Container plane ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mount {
    pub container_path: String,
    pub host_path: String,
    pub readonly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContainerConfig {
    pub name: String,
    pub attempt: u32,
    /// CAS vocabulary: a ref name or digest-hex into the image plane.
    pub image_ref: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_dir: String,
    #[serde(default)]
    pub envs: Vec<(String, String)>,
    #[serde(default)]
    pub mounts: Vec<Mount>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub log_path: String,
    // v1.1 additions
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub stdin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContainerState {
    Created,
    Running,
    Exited,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContainerStatus {
    pub id: ContainerId,
    pub sandbox: SandboxId,
    pub config: ContainerConfig,
    pub state: ContainerState,
    pub created_at_nanos: i64,
    /// 0 = never started
    pub started_at_nanos: i64,
    /// 0 = still running / never started
    pub finished_at_nanos: i64,
    /// valid only when state == Exited
    pub exit_code: i32,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
}

// ── Records ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerStatsRec {
    pub id: ContainerId,
    pub timestamp_nanos: i64,
    pub cpu_usage_core_nanos: u64,
    pub memory_working_set_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PulledImage {
    pub ref_name: String,
    pub root_hex: String,
    pub total_size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRecord {
    pub id: String,
    pub ref_name: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsInfo {
    pub timestamp_nanos: i64,
    pub mountpoint: String,
    pub used_bytes: u64,
    pub inodes_used: u64,
}

// ── Streaming sessions (v1.1 §B) ─────────────────────────────────────────────

/// Exit waiter — consumed once.
pub trait ExitWaiter: Send {
    fn wait(self: Box<Self>) -> Result<i32>;
}

/// Live I/O of an exec or attach session. tty=true → stdout carries the
/// pty stream, stderr is None, pty_master enables TIOCSWINSZ resize.
pub struct StreamSession {
    pub stdin: Option<std::fs::File>,
    pub stdout: Option<std::fs::File>,
    pub stderr: Option<std::fs::File>,
    pub pty_master: Option<std::fs::File>,
    pub waiter: Box<dyn ExitWaiter>,
}

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum BackendError {
    NotFound(String),
    AlreadyExists(String),
    InvalidArgument(String),
    FailedPrecondition(String),
    /// image present & referenced by live container (RemoveImage refusal)
    InUse(String),
    Internal(String),
    Io(std::io::Error),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::NotFound(m) => write!(f, "not found: {m}"),
            BackendError::AlreadyExists(m) => write!(f, "already exists: {m}"),
            BackendError::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
            BackendError::FailedPrecondition(m) => write!(f, "failed precondition: {m}"),
            BackendError::InUse(m) => write!(f, "in use: {m}"),
            BackendError::Internal(m) => write!(f, "internal: {m}"),
            BackendError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for BackendError {}

impl From<std::io::Error> for BackendError {
    fn from(e: std::io::Error) -> Self {
        BackendError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, BackendError>;

// ── Filters ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SandboxFilter {
    pub id: Option<SandboxId>,
    pub state: Option<SandboxState>,
    pub label_selector: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainerFilter {
    pub id: Option<ContainerId>,
    pub sandbox: Option<SandboxId>,
    pub state: Option<ContainerState>,
    pub label_selector: BTreeMap<String, String>,
}
