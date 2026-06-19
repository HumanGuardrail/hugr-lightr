//! lightr-index — frozen contract: build-spec v2 §5 (ADR-0010).
//! Stat-index + walk + snapshot/hydrate/status ops.
#![forbid(unsafe_code)]

mod index;

// Re-export all former-public items at crate root (API unchanged).
pub use index::codec::Index;
pub use index::gc::{gc, GcReport};
pub use index::hydrate::{hydrate, hydrate_verified, HydrateReport};
pub use index::scan::{scan, WalkReport};
pub use index::snapshot::{snapshot, SnapshotReport};
pub use index::status::{status, StatusReport};
pub use index::timeaxis::{bisect, diff_manifests, parse_lrr1, undo, DiffReport};

/// Process-global lock shared by ALL test modules that mutate LIGHTR_HOME.
/// Lives at crate level so both `mod tests` and `mod r1_tests` sub-files share
/// the same Mutex instance.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

