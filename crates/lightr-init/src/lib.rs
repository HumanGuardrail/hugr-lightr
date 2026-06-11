//! lightr-init — the Linux guest's PID 1 (build-spec-prod.md §WP-B-init).
//!
//! The LIBRARY is host-portable and fully unit-tested: the init lifecycle is
//! parameterized over `GuestOps` (mount/spawn) and `ExitSink` (exit-code
//! report), so it runs on Intel/macOS today. The real Linux syscalls + vsock
//! live in `bin/init.rs` behind `#[cfg(target_os = "linux")]`.
//!
//! This replaces the placeholder `exitCode = 0` in the vz shim: the guest
//! process's REAL exit code flows PID1 → ExitSink → host. Bodies: WP-B-init.

use serde::{Deserialize, Serialize};

/// What PID1 must do, as data — written by the host, read from a mounted spec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitSpec {
    pub command: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
}

impl InitSpec {
    pub fn from_json(_b: &[u8]) -> Result<Self, String> {
        todo!("WP-B-init")
    }
    pub fn to_json(&self) -> Vec<u8> {
        todo!("WP-B-init")
    }
}

/// Where PID1 reports the guest process exit code. Seam: tests use a Vec,
/// the real guest writes a vsock frame to the host.
pub trait ExitSink {
    fn report(&mut self, code: i32) -> std::io::Result<()>;
}

/// OS actions PID1 performs, seamed for host-side testing.
pub trait GuestOps {
    /// Mount a virtiofs share `tag` at `dest` (rootfs, store).
    fn mount_virtiofs(&mut self, tag: &str, dest: &str) -> std::io::Result<()>;
    /// Spawn the command, wait, return its exit code (128+signal on signal).
    fn spawn_wait(&mut self, cmd: &[String], cwd: &str, env: &[(String, String)])
        -> std::io::Result<i32>;
}

/// The init lifecycle: mount shares → spawn the command → report exit.
/// Returns the guest process exit code. Host-testable via Fake impls.
pub fn run_init<M: GuestOps>(
    _spec: &InitSpec,
    _ops: &mut M,
    _sink: &mut dyn ExitSink,
) -> std::io::Result<i32> {
    todo!("WP-B-init: mount rootfs+store, spawn_wait, sink.report(code), return code")
}
