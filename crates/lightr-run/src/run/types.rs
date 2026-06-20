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

#[derive(Default)]
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
    /// WP-RC-1 (R-KEY): user `-e`/`--env-file` env, as RESOLVED `(KEY, VALUE)`
    /// pairs. UNLIKE `env_keys` (var NAMES read from the process env at key
    /// time — the discovery channel), these are explicit literal values and are
    /// the ONLY env that enters the run memo key — folded `KEY=VALUE\0` in
    /// `assemble_key`/`build_key`. Empty ⇒ no contribution, so a run with no
    /// `-e`/`--env-file` keys byte-identically to before (behavior-preserving).
    pub env_explicit: Vec<(String, String)>,
    /// WP-RC-WORKDIR: user `-w`/`--workdir` — the working directory the run's
    /// process executes in (Docker `WORKDIR`). `None` ⇒ run in `cwd` (today's
    /// behaviour, byte-identical). `Some(path)` ⇒ run in `effective_cwd()` =
    /// `cwd.join(path)` (created on demand, like Docker creates `WORKDIR`).
    ///
    /// RUNTIME ONLY — never a memo-key input (like `ports`/limits; like Docker,
    /// which does not key on `-w`). It never enters `assemble_key`/`build_key`.
    pub workdir: Option<String>,
    /// WP-RC-USER: user `-u`/`--user` — the POSIX identity the run's process
    /// executes as (Docker `--user`). `None` ⇒ run as the current user (today's
    /// behaviour, byte-identical). `Some(spec)` ⇒ `uid[:gid]` (numeric) or
    /// `name[:group]` (best-effort) applied to the native child before exec
    /// (cfg(unix) only; a POSIX uid has no meaning on Windows).
    ///
    /// RUNTIME ONLY — never a memo-key input (like `ports`/`workdir`; like
    /// Docker, which does not key on `-u`). It never enters
    /// `assemble_key`/`build_key`.
    pub user: Option<String>,
}

impl RunSpec {
    /// The directory the run's process actually executes in (WP-RC-WORKDIR).
    /// A `workdir`-less run returns `cwd` unchanged (behavior-preserving); a set
    /// `workdir` resolves against `cwd` (a relative path joins; an absolute path
    /// replaces — `PathBuf::join` semantics, matching Docker's absolute-`WORKDIR`).
    pub fn effective_cwd(&self) -> std::path::PathBuf {
        match &self.workdir {
            Some(w) => self.cwd.join(w),
            None => self.cwd.clone(),
        }
    }
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

// R-MOUNT / R-SPECDISK (parity-contract.md §0): the go-forward, proto/kind-tagged
// on-disk mount shape. Mirrors `run::mount::MountKind`. The legacy `MountOnDisk`
// above stays for read back-compat; `MountOnDisk2` is what new spec.json writes.
// `#[serde(tag = "kind")]` makes it a tagged enum on disk. PARSING/RESOLUTION
// behaviour is WP-VOL-1's job — this only freezes the serialized SHAPE.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind")]
pub(super) enum MountOnDisk2 {
    CasRef {
        ref_name: String,
        target: String,
        #[serde(default)]
        readonly: bool,
    },
    HostBind {
        source: String,
        target: String,
        #[serde(default)]
        readonly: bool,
    },
    NamedVolume {
        source: String,
        target: String,
        #[serde(default)]
        readonly: bool,
    },
    AnonVolume {
        target: String,
        #[serde(default)]
        readonly: bool,
    },
    Tmpfs {
        target: String,
        #[serde(default)]
        opts: Vec<String>,
    },
}

// R-SPECDISK (parity-contract.md §0): proto-tagged published-port shape. The
// legacy `ports: Vec<(u16,u16)>` stays for read back-compat (TCP-only); `ports2`
// carries the protocol so UDP can land without a second format bump. Behaviour
// (binding UDP) is a Networking-axis WP's job.
#[derive(Serialize, Deserialize)]
pub(super) struct PortOnDisk {
    pub host: u16,
    pub container: u16,
    /// `"tcp"` (default) or `"udp"`.
    #[serde(default = "default_proto")]
    pub proto: String,
}

/// Serde default for [`PortOnDisk::proto`] — TCP, matching the legacy
/// `ports: Vec<(u16,u16)>` channel which was TCP-only.
pub fn default_proto() -> String {
    "tcp".to_string()
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

    // ── R-SPECDISK (parity-contract.md §0) — additive Docker-parity fields. ──
    // ALL `#[serde(default)]` for back-compat with spec.json written before the
    // freeze-gate. The existing `env`/`mounts`/`ports` above are UNTOUCHED. The
    // population + behaviour of every field below is a Wave-A/B WP's job; the
    // freeze-gate only lands the SHAPE.
    //
    // LEAD ARBITRATION (env-split): `env` above stays the UNKEYED discovery
    // channel; `env_explicit` below (user `-e`/`--env-file`) is the ONLY env
    // that enters the memo key (R-KEY). Two distinct channels — never merged.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub rm: bool,
    #[serde(default)]
    pub restart: Option<String>,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    /// User `-e`/`--env-file` env — the ONLY env in the memo key (R-KEY).
    #[serde(default)]
    pub env_explicit: Vec<(String, String)>,
    #[serde(default)]
    pub stop_signal: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub network_alias: Vec<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub add_host: Vec<(String, String)>,
    #[serde(default)]
    pub dns: Vec<String>,
    /// Go-forward tagged mount shape (R-MOUNT). Legacy `mounts` above stays for
    /// read back-compat.
    #[serde(default)]
    pub mounts2: Vec<MountOnDisk2>,
    /// Go-forward proto-tagged port shape. Legacy `ports` above stays for read
    /// back-compat (TCP-only).
    #[serde(default)]
    pub ports2: Vec<PortOnDisk>,
}

/// Serde default for [`SpecOnDisk::engine`] — the native supervisor branch, so a
/// pre-WP-NET2 spec.json (no `engine` field) keeps its original behaviour.
pub fn default_engine() -> String {
    "native".to_string()
}

// R-SPECDISK (parity-contract.md §0): a manual `Default` whose field values
// MATCH the serde defaults exactly (notably `engine = "native"`, NOT the empty
// string a derive would give). This lets every existing `SpecOnDisk { … }`
// construction site append `..Default::default()` for the additive freeze-gate
// fields without touching any field it already sets — zero behaviour change.
impl Default for SpecOnDisk {
    fn default() -> Self {
        SpecOnDisk {
            cwd: String::new(),
            command: Vec::new(),
            env_keys: Vec::new(),
            mounts: Vec::new(),
            detached: false,
            created_at_unix: 0,
            ports: Vec::new(),
            engine: default_engine(),
            rootfs_ref: None,
            env: Vec::new(),
            name: None,
            rm: false,
            restart: None,
            labels: Vec::new(),
            workdir: None,
            user: None,
            entrypoint: None,
            env_explicit: Vec::new(),
            stop_signal: None,
            network: None,
            network_alias: Vec::new(),
            hostname: None,
            add_host: Vec::new(),
            dns: Vec::new(),
            mounts2: Vec::new(),
            ports2: Vec::new(),
        }
    }
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
