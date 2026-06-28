//! lightr-engine submodules — Engine trait, dispatch, and all engine impls.

pub mod envuser;
pub mod kind;
pub mod native;
pub mod ns;
pub mod probe;
// WP-#108 (seccomp): OCI seccomp profile → cBPF compiler + apply, consumed by the
// `ns` engine (PID 1). Linux-only (the whole module is `#![cfg(target_os = "linux")]`).
#[cfg(target_os = "linux")]
pub(crate) mod seccomp;
pub mod spec;
// WP-#114: rootless subuid/subgid RANGE resolution (real non-root `--user`),
// consumed by the `ns` engine. Pure std (parse + helper-find); Linux-only because
// only the `ns` engine uses it (gating avoids dead-code on other targets).
#[cfg(target_os = "linux")]
pub(crate) mod subid;
pub mod vz;
pub mod wsl;

pub use kind::{EngineCaps, EngineKind};
pub use native::NativeEngine;
pub use probe::{pack_status, probe};
pub use spec::{BindMount, ExecSpec, MountKind, ResolvedMount, TmpfsMount, Ulimit};

use lightr_core::{LightrError, Result};

// ── Engine trait ──────────────────────────────────────────────────────────────

pub trait Engine {
    /// Spawn + wait; stdout/stderr inherit. Exit law: code or 128+signal.
    fn run(&self, spec: &ExecSpec) -> Result<i32>;
}

// ── engine_for ────────────────────────────────────────────────────────────────

/// Unavailable ⇒ Err(InvalidRef("engine <kind>: <probe detail>")).
pub fn engine_for(kind: EngineKind) -> Result<Box<dyn Engine>> {
    let caps = probe(kind);
    if !caps.available {
        return Err(LightrError::InvalidRef(format!(
            "engine {:?}: {}",
            kind, caps.detail
        )));
    }
    match kind {
        EngineKind::Native => Ok(Box::new(NativeEngine)),
        EngineKind::Ns => Ok(ns::ns_engine_box()),
        EngineKind::Vz => Ok(vz::vz_engine_box()),
        EngineKind::Wsl => Ok(wsl::wsl_engine_box()),
    }
}
