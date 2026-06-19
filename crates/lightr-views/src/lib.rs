//! lightr-views — O(1) materialization (ADR-0013), the other half of the
//! perf headline. The PLAN + SOLIDIFIER logic is pure and host-tested.
//!
//! # Shipped materialization vs. planned O(1) backends
//!
//! The **shipped** materialization path is CoW hydrate via `lightr_index`
//! (lightr_index::hydrate). It is the runtime today.
//!
//! The **planned** O(1) view backends — composefs/EROFS (Linux), NFS-loopback
//! (macOS, the EdenFS-proven route), and ProjFS (Windows) — are `cfg`-gated
//! modules in this crate. They are **intentionally not yet wired into the run
//! path**: wiring them in is the ADR-0013 S1/S3 spike work, gated on
//! target-box validation. Until then every backend method returns
//! `ErrorKind::Unsupported` so the honest state is always visible. Bodies: WP-W5.
//!
//! # The contract (pre-decided law)
//!
//! * [`plan_view`] walks every manifest entry (path-sorted) into a
//!   [`ViewPlan`]. The plan is the **O(1)-appearance** contract: building it
//!   never touches disk — it only records enough per entry (path, kind, and
//!   the file digest) to later drive fault-in and solidification.
//! * [`Solidifier`] is the heart and is **pure + fully host-tested**. It owns
//!   the promote-on-access policy and the "is the mount allowed to evaporate
//!   yet?" question. See [`Solidifier`] for the documented policy.
//! * [`ViewBackend`] is the seam to the OS. [`FakeBackend`] is the host-test
//!   double; the real backends are `cfg`-gated planned implementations (see
//!   the platform `composefs`/`nfsloopback`/`projfs` modules) that compile
//!   but are **intentionally not yet wired into the run path** (ADR-0013).

// The pure logic carries no `unsafe`. We deliberately do NOT
// `#![forbid(unsafe_code)]` at the crate root: the cfg-gated real backends
// (NFS-loopback server, EROFS/composefs layout) will need localized `unsafe`
// for the syscall/FFI boundary, and that is contained inside those modules.

mod views;

pub use views::backend::{solidify_step, FakeBackend, Solidifier, ViewBackend};
pub use views::plan::{plan_view, EntryKind, PlanEntry, ViewPlan};

#[cfg(target_os = "linux")]
#[path = "views/composefs.rs"]
pub mod composefs;

#[cfg(target_os = "macos")]
#[path = "views/nfsloopback.rs"]
pub mod nfsloopback;

#[cfg(target_os = "windows")]
#[path = "views/projfs.rs"]
pub mod projfs;

#[cfg(test)]
#[path = "views/tests.rs"]
mod tests;
