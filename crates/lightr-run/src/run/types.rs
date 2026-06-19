//! Run-time types: RunSpec, RunHandle, RunInfo, RunOutcome, PortMap, Mount,
//! StoreFile, LogStream, VzMemoKey, DeepMemoConfig, SpecOnDisk, MountOnDisk,
//! default_engine.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A published-port mapping: `host` (on 127.0.0.1) → `container` (on 127.0.0.1
/// where the run's server listens). TCP only in v1 (Networking Phase 1).
///
/// Ports are a **runtime** parameter, NOT a memo-key input — exactly like
/// resource limits, and exactly like Docker, which does not key on `-p`. They
/// never enter `build_key`/`assemble_key` (see the `ports_excluded_from_key`
/// test).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortMap {
    pub host: u16,
    pub container: u16,
}

pub struct RunSpec {
    pub cwd: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub command: Vec<String>,
    pub env_keys: Vec<String>,
    // R1: mounts hydrated CoW into <cwd>/<target> pre-key/pre-exec
    // (build-spec-r1 §2); part of the memo key in order.
    pub mounts: Vec<Mount>,
    // F-309 (build-spec-parity.md §0/§A0.2): store-backed inputs. IN the memo
    // key (a different secret/config ⇒ a different run). Hydrated on miss to
    // <cwd>/.lightr/secrets/<name> (0600) / <cwd>/.lightr/configs/<name> (0644).
    pub secrets: Vec<StoreFile>,
    pub configs: Vec<StoreFile>,
    // Networking Phase 1: published host→container TCP ports. RUNTIME ONLY —
    // never part of the memo key (like resource limits; like Docker `-p`). The
    // detached supervisor publishes each entry by forwarding 127.0.0.1:host →
    // 127.0.0.1:container for the run's lifetime.
    pub ports: Vec<PortMap>,
}

pub struct Mount {
    pub ref_name: String,
    pub target: String,
}

/// A store-backed file injected into a run. `ref_name` resolves via lightr_index.
pub struct StoreFile {
    pub name: String,
    pub ref_name: String,
}

pub struct RunOutcome {
    pub key: lightr_core::Digest,
    pub hit: bool,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub struct RunHandle {
    pub id: String,
    pub dir: std::path::PathBuf,
}

pub struct RunInfo {
    pub id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub command: Vec<String>,
    pub created_at_unix: u64,
    /// F-309: last healthcheck verdict from `<run_dir>/health`, if a
    /// healthcheck was configured for this run. `None` ⇒ no healthcheck (the
    /// common case). NOT part of the memo key.
    pub health: Option<crate::healthcheck::Health>,
    /// WP-PS-ENRICH: the engine that ran this detached job ("native" or "vz").
    /// Sourced from `SpecOnDisk::engine`; defaults to "native" for old run dirs
    /// whose spec.json pre-dates the engine field (back-compat via serde default).
    pub engine: String,
    /// WP-PS-ENRICH: published host→container TCP port mappings. Empty for
    /// runs with no `-p` flags. Sourced from `SpecOnDisk::ports`.
    pub ports: Vec<(u16, u16)>,
    /// WP-PS-ENRICH: the rootfs ref the vz engine booted, if any. `None` for
    /// native runs. Sourced from `SpecOnDisk::rootfs_ref`.
    pub rootfs_ref: Option<String>,
}

pub enum LogStream {
    Stdout,
    Stderr,
    Both,
}

// ---------------------------------------------------------------------------
// SpecOnDisk — private serde mirror for spec.json
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub(super) struct MountOnDisk {
    pub ref_name: String,
    pub target: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct SpecOnDisk {
    pub cwd: String,
    pub command: Vec<String>,
    pub env_keys: Vec<String>,
    pub mounts: Vec<MountOnDisk>,
    pub detached: bool,
    pub created_at_unix: u64,
    // Networking Phase 1: published (host, container) TCP ports the supervisor
    // forwards. `#[serde(default)]` keeps JSON back-compat: spec.json files
    // written before this field existed (no `ports`) still parse to an empty
    // Vec, so an old detached run never breaks on read.
    #[serde(default)]
    pub ports: Vec<(u16, u16)>,
    // WP-NET2: the engine that runs this detached job. `#[serde(default)]` →
    // "native" for spec.json files written before this field existed, so an old
    // detached run keeps the native supervisor branch. The vz branch (a Linux
    // container in a microVM, with host→guest port forwarding) is selected by
    // engine == "vz" AND a present `rootfs_ref`.
    #[serde(default = "default_engine")]
    pub engine: String,
    // WP-NET2: the rootfs ref the vz branch hydrates + boots. None for native
    // runs (serde default). Present ⇒ a vz container run.
    #[serde(default)]
    pub rootfs_ref: Option<String>,
    /// WP-DISC: explicit env vars set on the detached child (compose service
    /// discovery: <PEER>_HOST/<PEER>_PORT). serde-defaulted = back-compat. NOT a
    /// memo-key input (runtime addressing, like ports) — and detached runs aren't
    /// memoized anyway.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// Serde default for [`SpecOnDisk::engine`] — the native supervisor branch, so a
/// pre-WP-NET2 spec.json (no `engine` field) keeps its original behaviour.
pub fn default_engine() -> String {
    "native".to_string()
}

// ---------------------------------------------------------------------------
// R4 additions — frozen contract: build-spec-r4.md §1 (bodies: R4-W1)
// ---------------------------------------------------------------------------

/// Deep-memo (opt-in nitro, ADR-0016): process-tree memoization via a
/// spawn-shim. Degrades HONESTLY to whole-run memo when the shim can't
/// attach (SIP/static binaries) — never silently claims the capability.
pub struct DeepMemoConfig {
    pub enabled: bool,
}

/// Inputs that identify a memoizable `vz` container run. A different command,
/// rootfs image, or env ⇒ a different run ⇒ a different key.
///
/// `rootfs_digest` is the resolved content digest of the rootfs image (the
/// ref's current root), so two refs pointing at the same content share a memo
/// entry and a ref re-pointed at new content misses — exactly like a mount's
/// key contribution in `assemble_key`.
pub struct VzMemoKey {
    pub command: Vec<String>,
    pub rootfs_digest: lightr_core::Digest,
    pub env: Vec<(String, String)>,
}
