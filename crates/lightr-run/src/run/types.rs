//! Run-time types: RunSpec, RunHandle, RunInfo, RunOutcome, PortMap, Mount,
//! StoreFile, LogStream, VzMemoKey, DeepMemoConfig. The on-disk serde mirror
//! (SpecOnDisk, MountOnDisk(2), PortOnDisk, default_engine/default_proto) lives
//! in `specdisk.rs`, re-exported here so `types::SpecOnDisk` resolves as before.

use std::path::PathBuf;

/// A published-port mapping: `host` (on `host_ip`) → `container` (on 127.0.0.1
/// where the run's server listens, or the guest IP for a vz container). TCP
/// only in v1 (Networking Phase 1).
///
/// `host_ip` is the interface the forwarder BINDS the host port on (Docker's
/// `-p HOST_IP:HOST:CONTAINER`). Empty string means the default `0.0.0.0` (all
/// interfaces) — see [`PortMap::bind_ip`]. It is kept as an empty-able `String`
/// (rather than a parsed `IpAddr`) for two reasons: (1) `Default`/`Copy` stay
/// trivial for the many `PortMap { host, container, .. }` construction sites,
/// and (2) the bind site re-validates anyway. The CLI parser
/// (`flags_publish::parse_publish_spec`) already validates the IP grammar, so an
/// invalid string never reaches here on the user path.
///
/// Ports are a **runtime** parameter, NOT a memo-key input — exactly like
/// resource limits, and exactly like Docker, which does not key on `-p`. They
/// never enter `build_key`/`assemble_key` (see the `ports_excluded_from_key`
/// test).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PortMap {
    pub host: u16,
    pub container: u16,
    /// The host interface to bind the published port on. Empty ⇒ `0.0.0.0`
    /// (all interfaces, the default). A non-empty value is an IP literal
    /// (`127.0.0.1`, `::1`) validated upstream by the CLI parser.
    pub host_ip: String,
}

impl PortMap {
    /// Construct a `PortMap` binding the default host interface (`0.0.0.0`).
    /// Convenience for the common `host:container` (no explicit host-ip) case.
    pub fn new(host: u16, container: u16) -> Self {
        PortMap {
            host,
            container,
            host_ip: String::new(),
        }
    }

    /// The concrete interface the forwarder binds on: the configured `host_ip`,
    /// or `0.0.0.0` when empty (the default — all interfaces). Centralizes the
    /// empty-means-default rule so every bind site stays consistent.
    pub fn bind_ip(&self) -> &str {
        if self.host_ip.is_empty() {
            "0.0.0.0"
        } else {
            &self.host_ip
        }
    }
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
    /// WP-RESLIMITS: resource caps (`--memory`/`--cpus`; compose
    /// `deploy.resources.limits`). `Default` ⇒ unlimited (every field `None`), so
    /// a run that sets no caps is byte-identical to before. Carried to the detached
    /// supervisor + the synchronous memo exec, where the enforceable part is
    /// applied: `RLIMIT_AS`/`RLIMIT_DATA` for `memory_bytes` on Linux (a hard cap
    /// — an over-cap child is killed); `cpu_millis` is NOT a portable native cap
    /// (`RLIMIT_CPU` is total cpu-seconds, not a share) so it is RECORDED + honestly
    /// surfaced, never silently pretended-enforced on the native engine.
    ///
    /// RUNTIME ONLY — never a memo-key input (like `ports`/`workdir`; like Docker,
    /// which does not key on `--memory`/`--cpus`). It never enters
    /// `assemble_key`/`build_key`.
    pub limits: lightr_core::ResourceLimits,

    /// WP-RUNFLAGS: Docker `-v/--volume SRC:DST[:ro]` host binds. A bind makes a
    /// HOST path visible at `cwd/<target>` (the native run's "container root" is
    /// its cwd — same relative-target law as `mounts`). RUNTIME ONLY (a host path
    /// is non-deterministic): never keyed, and its presence forces a memo MISS
    /// with NO Action-Cache write (a bind run can't be replayed). `Default` ⇒
    /// empty ⇒ byte-identical to before. Native realization: rw ⇒ a symlink
    /// (live view); ro ⇒ a read-only snapshot copy (no mount namespace on native).
    pub volumes: Vec<VolumeBind>,
    /// WP-RUNFLAGS: Docker `--tmpfs DST` — an empty writable dir at `cwd/<target>`.
    /// RUNTIME ONLY (writable scratch is non-deterministic): never keyed, forces a
    /// memo MISS with no AC write. `Default` ⇒ empty ⇒ byte-identical to before.
    pub tmpfs: Vec<String>,
    /// WP-RUNFLAGS: Docker `--entrypoint CMD` — override the exec entrypoint for
    /// this run. On the native path the effective argv is `[entrypoint] ++ command`
    /// (Docker prepends the entrypoint to CMD). `None` ⇒ `command` unchanged
    /// (byte-identical to before). RUNTIME ONLY on the native path (the run key
    /// already folds the full `command`; the entrypoint prepends to it at exec).
    pub entrypoint: Option<Vec<String>>,
    /// WP-RUNFLAGS: Docker `--name NAME` — the detached run's name, claimed in the
    /// name→id registry on spawn so `ps`/`stop`/`logs`/`rm` resolve it. `None` ⇒
    /// no name (byte-identical to before). RUNTIME ONLY (never keyed). Detached
    /// runs only (a foreground run has no run dir/id to name).
    pub name: Option<String>,
    /// WP-RUNFLAGS: Docker `--rm` — auto-remove the detached run's dir + release
    /// its name when the supervisor exits. `false` ⇒ the run dir persists (today's
    /// behaviour). RUNTIME ONLY (never keyed). Detached runs only.
    pub rm: bool,

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

    // ── WP-C9 (ADR-0018) — vz container-networking carry-fields. RUNTIME-ONLY
    // (like every RC-SEAM field above; like Docker, which keys on none of
    // `--network`/`--network-alias`/`--add-host`/`--dns`). NONE enters
    // `assemble_key`/`build_key`. `#[derive(Default)]` gives each its no-op
    // default (`None`/empty), so a run that joins no network behaves EXACTLY as
    // today — the single-NAT-NIC vz path is byte-identical. The detached `vz`
    // supervisor reads these back (via `SpecOnDisk`) and, when `network` is
    // `Some`, joins the per-network registry + attaches the shared L2 switch
    // (mesh NIC `eth1`), keeping `eth0` (NAT egress) unchanged.
    /// `--network <name>`: the user network this vz run joins. `None` ⇒ no mesh
    /// NIC (today's single-NAT-NIC path, byte-identical).
    pub network: Option<String>,
    /// `--network-alias`: extra DNS names this member answers to on the network
    /// (alongside its run/`--name`). Empty ⇒ name-only.
    pub network_alias: Vec<String>,
    /// `--add-host HOST:IP`: extra `/etc/hosts` entries, as raw `"host:ip"`
    /// strings (parsed to `(host, ip)` pairs at the vz wiring site). Empty ⇒
    /// none.
    pub add_host: Vec<String>,
    /// `--dns`: resolver addresses written into the guest's `/etc/resolv.conf`.
    /// Empty ⇒ the network's embedded resolver / host upstream, as before.
    pub dns: Vec<String>,
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

/// WP-RUNFLAGS: a resolved Docker `-v/--volume` host bind. `source` is the HOST
/// path (canonicalization deferred to materialization); `target` is the relative
/// in-cwd destination (same law as [`Mount::target`]); `readonly` is the `:ro`
/// option. Distinct from [`Mount`] (a CAS ref hydrated into cwd) — this binds a
/// LIVE host path. Carried on `RunSpec.volumes`, never keyed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeBind {
    pub source: String,
    pub target: String,
    pub readonly: bool,
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
// re-exported. WP-B2 adds `PortOnDisk`/`default_proto`: `spawn.rs` writes the
// go-forward host-ip-tagged `ports2` channel and the supervisors read it back.
pub(super) use specdisk::{default_proto, MountOnDisk, MountOnDisk2, PortOnDisk, SpecOnDisk};

/// WP-RUNFLAGS: lower a `RunSpec`'s `-v/--volume` host binds + `--tmpfs` dirs
/// into the persisted, tagged `mounts2` shape so the detached supervisor reads
/// them back and materializes them. Empty in / empty out (behaviour-preserving:
/// a run with no `-v`/`--tmpfs` writes no `mounts2`). The legacy CAS-ref `mounts`
/// channel stays on the separate `mounts` field — these two never mix.
pub(super) fn mounts2_from_runspec(spec: &RunSpec) -> Vec<MountOnDisk2> {
    let mut out: Vec<MountOnDisk2> = Vec::new();
    for v in &spec.volumes {
        out.push(MountOnDisk2::HostBind {
            source: v.source.clone(),
            target: v.target.clone(),
            readonly: v.readonly,
        });
    }
    for t in &spec.tmpfs {
        out.push(MountOnDisk2::Tmpfs {
            target: t.clone(),
            opts: Vec::new(),
        });
    }
    out
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
