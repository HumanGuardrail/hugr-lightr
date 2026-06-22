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
//! RUN against the real implemented container/exec/image/stats methods (the
//! sandbox prefix is satisfied by the test scaffold); every `Defer*` is gated
//! out and LOGGED (never silently skipped):
//!   - `DeferSandbox` — asserts sandbox-plane semantics (state/cascade/ip),
//!     fail-closed in LightrBackend (WP-CRI-SANDBOX).
//!   - `DeferStream`  — uses `open_exec` (streaming), fail-closed (WP-CRI-STREAM).
//!   - `DeferLog`     — asserts the CRI log file (needs the sandbox
//!     `log_directory` wiring, WP-CRI-SANDBOX).
//!   - `DeferNet`     — image-CONTENT pull of a synthetic ref: the fake
//!     fabricates the record in-memory, the real backend performs a live OCI
//!     registry pull. A seam reality (no network in the gate), not a fixable
//!     divergence.

/// Why a vector is gated out of the RUN set (logged, never silently skipped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// Runs against the real implemented methods (sandbox prefix scaffolded).
    RunLifecycle,
    /// Sandbox-plane semantics — fail-closed (WP-CRI-SANDBOX).
    DeferSandbox,
    /// Streaming `open_exec` — fail-closed (WP-CRI-STREAM).
    DeferStream,
    /// CRI log-file assertion — needs sandbox `log_directory` (WP-CRI-SANDBOX).
    DeferLog,
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
