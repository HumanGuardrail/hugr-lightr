//! Resource-limit application for the memoized native spawn (F-203).
//!
//! build-spec-parity.md §A0.3 freezes these seams; **WP-A1 fills the bodies**
//! (Unix `pre_exec` + `libc::setrlimit` for memory; cgroup v2 for the `ns`
//! engine). A0 ships honest no-op stubs so the call sites compile and every
//! existing test stays green — they must NOT silently pretend to enforce a cap.

use lightr_core::{ResourceLimits, Result};

/// Apply resource caps to a not-yet-spawned `Command` (memoized native path).
///
/// A0 stub: inert no-op (`Ok(())`). WP-A1 installs a `pre_exec` hook that calls
/// `setrlimit(RLIMIT_AS/RLIMIT_DATA)` for `memory_bytes` and returns an honest
/// `Unsupported` error when a cpu share is requested on the native engine.
pub fn apply_native(cmd: &mut std::process::Command, limits: &ResourceLimits) -> Result<()> {
    // Reference both params so the frozen seam is unmistakable and clippy stays
    // quiet until WP-A1 replaces this body.
    let _ = (cmd, limits);
    Ok(())
}

/// Apply resource caps via cgroup v2 (the `ns` engine, Linux). A0 stub: no-op.
/// WP-A1 writes `memory.max` / `cpu.max` into a delegated subtree, or returns an
/// honest `Unsupported` error when cgroup v2 is unavailable / not delegated.
pub fn apply_cgroup(limits: &ResourceLimits) -> Result<()> {
    let _ = limits;
    Ok(())
}
