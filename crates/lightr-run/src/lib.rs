//! lightr-run — frozen contract: build-spec v2 §6 + build-spec-r1 §2.
//! Memo key, native exec, replay, supervisor, ps, logs, stop, exec_in.

// Per-feature seam modules (build-spec-parity.md §1). A0 wires the call sites
// to honest stubs in these files; A1/A3 fill the bodies.
pub mod healthcheck;
pub mod limits;
// F-308 (build-spec-parity.md §3): PURE OS-supervisor unit-file templates +
// RestartPolicy. No I/O lives here; the install/uninstall/list flow is in
// lightr-cli::handlers::supervise. We ship NO daemon — we generate a unit and
// tell the user the opt-in command.
pub mod portforward;
pub mod restart;
pub mod secrets;

// F-304 Phase-2 (ADR-0018): daemonless userspace L2 switch for vz container
// networking (container↔container, name-DNS, udp). CONTRACT STUB — the C-wave
// (C1 network / C2 switch / C3 dhcp / C4 dns / C5 runtime) fills the bodies.
// unix-only (RawFd + datagram sockets); windows networking is a future ring.
#[cfg(unix)]
pub mod network;
#[cfg(unix)]
pub mod vswitch;

// Internal run submodules — all public surfaces re-exported below.
mod run;

// ---------------------------------------------------------------------------
// Re-export every former-public type and function so external crates compile
// unchanged. Items that were pub(crate) in the flat lib.rs are re-exported
// pub here to preserve the crate's public API surface exactly.
// ---------------------------------------------------------------------------

// types (public surface only — MountOnDisk, SpecOnDisk, default_engine were
// private in the flat lib.rs and remain so inside run::types as pub(super))
pub use run::types::{
    DeepMemoConfig, LogStream, Mount, PortMap, RunHandle, RunInfo, RunOutcome, RunSpec, StoreFile,
    VzMemoKey,
};

// R-MOUNT (parity-contract.md §0): frozen volume TYPES. PARSING is WP-VOL-1.
// (MountOnDisk2 / PortOnDisk are pub(super) serde-mirror types in run::types,
// used by SpecOnDisk — not re-exported, exactly like MountOnDisk.)
pub use run::mount::{parse_mount_long, parse_tmpfs, parse_v, MountKind, MountSpec, ResolvedMount};

// registry (WP-LIFE-01) — name→id registry API; consumed by the CLI-lifecycle
// wiring WPs (LIFE-02/03 --name + verb name-resolution).
pub use run::registry::{claim, name_validate, release, resolve};

// lifecycle (SKELETON-FREEZE) — run-instance lifecycle PRIMITIVES consumed by
// the container-verb handlers (LIFE-02..20: rm/kill/start/restart/wait/stop).
// Each verb resolves an id via `registry::resolve` then calls one primitive, so
// the verbs stay disjoint (no shared-file collisions, no duplicated run-dir
// logic). NOT dead-code: lightr-cli's verb handlers are the consumers.
pub use run::lifecycle::{remove_run, respawn_run, run_status, signal_run, wait_run, RunStatus};

// memo
pub use run::memo::{predict, run_memoized, run_memoized_with};

// vzmemo
pub use run::vzmemo::{run_vz_memoized, vz_memo_key};

// deepmemo
pub use run::deepmemo::{deep_memo_available, run_memoized_deep};

// spawn
pub use run::spawn::{spawn_detached, spawn_detached_engine, spawn_detached_with_health};

// supervise
pub use run::supervise::supervise;

// ps
pub use run::ps::ps;

// logs
pub use run::logs::logs;

// stop
pub use run::stop::stop;

// exec
pub use run::exec::exec_in;
