//! Resource-limit application for the engine spawns (F-203).
//!
//! build-spec-parity.md §A0.4 freezes these seams; **WP-A1 fills the bodies**
//! (native: `pre_exec` + `libc::setrlimit`; ns: cgroup v2). This is a sibling of
//! `lightr-run`'s `limits.rs`: run's applies to a std `Command` pre-output;
//! engine's applies to the engine's own spawns. A0 ships honest no-op stubs so
//! the call sites compile and every existing test stays green — they must NOT
//! silently pretend to enforce a cap.

use lightr_core::{ResourceLimits, Result};

/// Apply resource caps to a not-yet-spawned engine `Command` (native engine).
/// A0 stub: inert no-op (`Ok(())`). WP-A1 installs the `setrlimit` `pre_exec`.
pub fn apply_native(cmd: &mut std::process::Command, limits: &ResourceLimits) -> Result<()> {
    let _ = (cmd, limits);
    Ok(())
}

/// Apply resource caps via cgroup v2 (the `ns` engine, Linux). A0 stub: no-op.
/// WP-A1 writes `memory.max` / `cpu.max`, or returns an honest `Unsupported`
/// error when cgroup v2 is unavailable / not delegated.
pub fn apply_cgroup(limits: &ResourceLimits) -> Result<()> {
    let _ = limits;
    Ok(())
}
