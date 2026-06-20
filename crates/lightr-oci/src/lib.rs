//! lightr-oci — frozen contract: build-spec-r2.md §3 (bodies: WP R2-W1).
//! BRIDGE crate: the only place network code may live (ADR-0011).
//!
//! # sha256 ↔ Digest mapping (R2-HARDEN)
//!
//! `lightr_core::Digest` is a 32-byte wrapper (`[u8;32]`) that normally holds
//! BLAKE3 output. SHA-256 also produces exactly 32 bytes, so we store the raw
//! sha256 bytes directly in the `Digest` wrapper without any re-hashing.
//! When emitting `LightrError::Integrity { expected, actual }` for an OCI
//! blob mismatch the `Display` impl therefore prints a 64-char sha256 hex —
//! it will NOT match a BLAKE3 hex from the rest of the codebase. We annotate
//! every such callsite with `// sha256 bytes stored in Digest (not blake3)`.
//! The error message from `verify_sha256_digest` additionally prefixes the
//! context string with "sha256:" so operators see the algorithm at a glance.
//!
//! # Exit-code mapping (LightrError → CLI exit code)
//!
//! The mapping is owned by lightr-cli's `die_lightr`:
//!   - `Integrity`           → exit 1 (content-hash mismatch: real corruption)
//!   - `InvalidManifest`     → exit 1 (structural parse error)
//!   - `InvalidRef`          → exit 2 (usage/bad-ref: caller error)
//!   - `RefNotFound`         → exit 2
//!   - `NotFound`/`TooLarge` → exit 1
//!   - `Io`                  → exit 1
//!   - `Registry`            → exit 1 (HTTP-protocol/auth/rate-limit/5xx)
//!
//! "bad layout/name ⇒ 2" (spec §4) means a USAGE error: the caller supplied an
//! invalid ref name or a nonsensical image ref (empty repo, bad chars). Those
//! return `InvalidRef`. Structural layout errors (missing blobs, parse failures)
//! are `InvalidManifest` → exit 1, which is correct: the layout exists but is
//! broken, not a caller mistake.

#![forbid(unsafe_code)]
// ureq::Error is a large enum (272+ bytes) that we cannot shrink — the lint
// fires on every closure that calls req.call(). Suppressed crate-wide because
// the alternative (Box<ureq::Error>) would infect all callers of retry_request.
#![allow(clippy::result_large_err)]

mod oci;

// ── Public API re-exports ─────────────────────────────────────────────────

pub use oci::images::{list_images, ImageRow};
pub use oci::import::import_layout;
pub use oci::load::load;
pub use oci::model::{ImportReport, LoadReport, PushReport, SaveReport};
pub use oci::pull::pull;
pub use oci::push::push;
pub use oci::rmi::{render_rmi_results, rmi_many, rmi_one, RmiReport, RmiResult};
pub use oci::save::save;
