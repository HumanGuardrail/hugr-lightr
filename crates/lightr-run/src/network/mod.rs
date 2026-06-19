//! Network registry for vz container networking (ADR-0018, F-304 Phase-2).
//!
//! Per-network membership + deterministic addressing, persisted under
//! `$LIGHTR_HOME/net/<id>/` and `flock`-guarded (mirroring the gc lock law).
//! The registry is the source of truth the userspace L2 switch reads for its
//! MAC-learning seed, DHCP leases, and DNS name table.
//!
//! CONTRACT STUB (ADR-0018, WP-C1): the signatures + types below are frozen.
//! WP-C1 fills the bodies and REMOVES the `#![allow]` line.
//!
//! ## Locking discipline (mirrors `lightr-store`'s gc lock law)
//!
//! Each network dir owns a `<id>/.lock` file. Mutators (`create` persist,
//! `join`, `leave`) take an EXCLUSIVE advisory `flock` (`LOCK_EX`) for the
//! whole read-modify-write; readers (`members`) take a SHARED lock
//! (`LOCK_SH`). This is a real advisory `flock` (this module is `#[cfg(unix)]`
//! and `libc` is a dep), so concurrent supervisors joining/leaving the same
//! network serialize their `members.json` updates and never tear it. Writes
//! are atomic (temp + fsync + rename + parent fsync), so a crash mid-write
//! leaves the previous `members.json` intact. Corrupt JSON fails closed
//! (an `io::Error`), never a silent empty-membership.

mod alloc;
mod fsutil;
mod registry;
mod types;

pub use registry::NetworkRegistry;
pub use types::{MacAddr, Member, NetworkId, Subnet};
