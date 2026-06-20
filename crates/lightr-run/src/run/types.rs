//! Run-time types: RunSpec, RunHandle, RunInfo, RunOutcome, PortMap, Mount,
//! StoreFile, LogStream, VzMemoKey, DeepMemoConfig. The on-disk serde mirror
//! (SpecOnDisk, MountOnDisk(2), PortOnDisk, default_engine/default_proto) lives
//! in `specdisk.rs`, re-exported here so `types::SpecOnDisk` resolves as before.

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
    /// WP-RC-RESTART: user `--restart` — the Docker restart policy the detached
    /// supervisor applies on child exit (`no` | `always` | `on-failure[:max]` |
    /// `unless-stopped`). `None` ⇒ `no` (today's behaviour: the supervisor runs
    /// the child once and exits, byte-identical). `Some(spec)` is honored ONLY on
    /// the detached supervisor's re-spawn loop (a synchronous run returns once;
    /// the vz branch boots a microVM, not a re-spawnable native child).
    ///
    /// RUNTIME ONLY — never a memo-key input (like `ports`/`workdir`/`user`; like
    /// Docker, which does not key on `--restart`). It never enters
    /// `assemble_key`/`build_key`.
    pub restart: Option<String>,
    /// WP-RC-STOPSIGNAL: user `--stop-signal` — the signal `lightr stop` (and the
    /// restart-stop path) sends to gracefully stop the run, before the SIGKILL
    /// fallback (Docker `--stop-signal`/`STOPSIGNAL`). Numeric (`9`, `15`) or a
    /// portable name (`HUP`/`INT`/`QUIT`/`KILL`/`TERM`, case-insensitive, optional
    /// `SIG` prefix). `None` ⇒ SIGTERM (15), today's behaviour, byte-identical.
    ///
    /// RUNTIME ONLY — never a memo-key input (like `ports`/`workdir`/`user`/
    /// `restart`; like Docker, which does not key on `--stop-signal`). It never
    /// enters `assemble_key`/`build_key`.
    pub stop_signal: Option<String>,

    // ── RC-SEAM-FREEZE (skeleton-freeze) — additive RC carry-fields for the wide
    // runtime-config flag fan-out. EVERY field is RUNTIME-ONLY (like the fields
    // above; like Docker, which keys on none of these) — NONE enters
    // `assemble_key`/`build_key`. `#[derive(Default)]` gives each its no-op
    // default (`None`/empty/`false`), so a run that sets none behaves EXACTLY as
    // today. Each future RC WP fills ONE field (from its CLI flag) + the matching
    // `apply_<field>` stub in `run/apply_cfg.rs` — DISJOINT, one slot each.
    pub hostname: Option<String>,      // --hostname
    pub labels: Vec<(String, String)>, // --label/-l (key,value)
    pub cap_add: Vec<String>,          // --cap-add
    pub cap_drop: Vec<String>,         // --cap-drop
    pub privileged: bool,              // --privileged
    pub tty: bool,                     // -t/--tty
    pub init: bool,                    // --init (PID 1 zombie reaper)
    pub read_only: bool,               // --read-only rootfs
    pub oom_score_adj: Option<i32>,    // --oom-score-adj
    pub pids_limit: Option<i64>,       // --pids-limit (cgroup pids.max)
    pub shm_size: Option<u64>,         // --shm-size (/dev/shm bytes)
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
// SpecOnDisk — private serde mirror for spec.json (split into specdisk.rs to
// stay under the 400-line godfile cap). Re-exported so `types::SpecOnDisk` (and
// the on-disk mount/port shapes + serde defaults) resolve exactly as before.
// ---------------------------------------------------------------------------
#[path = "specdisk.rs"]
mod specdisk;
// Only the types referenced through `types::` by sibling `run` modules are
// re-exported. `MountOnDisk2`/`PortOnDisk`/`default_engine`/`default_proto` are
// used solely inside `specdisk.rs` (by `SpecOnDisk`'s fields + serde defaults).
pub(super) use specdisk::{MountOnDisk, SpecOnDisk};

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
