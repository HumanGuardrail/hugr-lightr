//! Conformance-vector DEFINITIONS — TRANSCRIBED from lightr-cri-vectors
//! @ seam-contract-v1.1 (the `vectors/*.json` corpus, FROZEN 2026-06-12).
//!
//! WIRE-LEVEL SEAM PROOF, NOT a git/path dep on lightr-cri (ADR-0017 decision 3,
//! the house seam pattern). Each const is a byte-for-byte copy of the
//! corresponding `lightr-cri/vectors/<name>.json`; drift between the two repos
//! is caught HERE (a parse/run failure), never by a crate import. The corpus is
//! split across `data_a.rs`/`data_b.rs` only to honor the 400-LOC godfile guard.
//!
//! Category drives the GREENLIST (`tests/vectors.rs`): `RunLifecycle` vectors
//! RUN against the REAL `LightrBackend` — the FULL plane (sandbox state machine,
//! container/exec/image/stats, streaming `open_exec`, CRI log file) is now wired
//! (WP-CRI-SANDBOX + WP-CRI-STREAM merged), so the sandbox/streaming/log vectors
//! are UN-DEFERRED and run directly (no scaffold). The ONLY remaining gated-out
//! class is `DeferNet`, LOGGED (never silently skipped):
//!   - `DeferNet` — needs a LIVE OCI registry (image-CONTENT pull of a synthetic
//!     ref): the fake fabricates the record in-memory, the real backend performs
//!     a live OCI registry pull. A seam reality (no network in the macOS gate),
//!     not a fixable divergence.
//!
//! PLATFORM NOTE (contract §5): the sandbox STATE-MACHINE vectors run on macOS
//! (the state machine + crash-only persistence are platform-uniform). The
//! netns/CNI RUNTIME is cfg(linux) and probe-truthful: on macOS a sandbox has
//! `ip = None` regardless of `host_network`, so `host-network-sandbox-no-ip`
//! (which asserts the IP is ABSENT) RUNS here; no vector asserts an
//! IP-PRESENT/CNI-assigned address, so nothing defers on the Linux-runtime axis.

/// Why a vector is gated out of the RUN set (logged, never silently skipped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// Runs against the REAL `LightrBackend` (full plane wired).
    RunLifecycle,
    /// Image-content pull of a synthetic ref — needs a live OCI registry.
    DeferNet,
}

/// One transcribed vector: its name, its GREENLIST category, and the verbatim
/// JSON from `lightr-cri/vectors/<name>.json`.
pub struct VectorDef {
    pub name: &'static str,
    pub category: Category,
    pub json: &'static str,
}

/// The full frozen corpus (29 vectors @ seam-contract-v1.1), concatenated from
/// the two godfile-split halves.
pub fn vectors() -> Vec<&'static VectorDef> {
    data_a::GROUP.iter().chain(data_b::GROUP.iter()).collect()
}

#[path = "data_a.rs"]
pub mod data_a;
#[path = "data_b.rs"]
pub mod data_b;
