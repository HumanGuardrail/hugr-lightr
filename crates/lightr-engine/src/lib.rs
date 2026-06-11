//! lightr-engine — frozen contract: build-spec-r2.md §2 (bodies: WP R2-W2).

use lightr_core::Result;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Native,
    Ns,
    Vz,
}

impl std::str::FromStr for EngineKind {
    type Err = lightr_core::LightrError;
    fn from_str(_s: &str) -> std::result::Result<Self, Self::Err> {
        todo!("R2-W2: native|ns|vz")
    }
}

pub struct EngineCaps {
    pub available: bool,
    pub detail: String,
}

/// Probe WITHOUT side effects (build-spec-r2 §2).
pub fn probe(_kind: EngineKind) -> EngineCaps {
    todo!("R2-W2")
}

pub struct ExecSpec<'a> {
    pub cwd: &'a Path,
    pub command: &'a [String],
    /// ns/vz: CoW-materialized tree to pivot/boot into. Native: must be None.
    pub rootfs: Option<&'a Path>,
}

pub trait Engine {
    /// Spawn + wait; stdout/stderr inherit. Exit law: code or 128+signal.
    fn run(&self, spec: &ExecSpec) -> Result<i32>;
}

/// Unavailable ⇒ Err(InvalidRef("engine <kind>: <probe detail>")).
pub fn engine_for(_kind: EngineKind) -> Result<Box<dyn Engine>> {
    todo!("R2-W2")
}
