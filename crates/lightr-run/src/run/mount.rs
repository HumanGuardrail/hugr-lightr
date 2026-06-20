//! Volume / mount TYPES — frozen by the FREEZE-GATE (parity-contract.md §0
//! R-MOUNT). This module freezes the SHAPES only; the PARSING + RESOLUTION
//! behaviour is WP-VOL-1's job (and the VOL-2..VOL-12 ring).
//!
//! The five Docker mount kinds, the pre-resolution [`MountSpec`] (what a CLI
//! `-v` / `--mount` / `--tmpfs` flag parses into) and the post-resolution
//! [`ResolvedMount`] (what `ExecSpec` carries to the engine) all land here so
//! the dependent WPs transcribe a frozen interface instead of designing one.
//!
//! Absolute-target rule (frozen, behaviour deferred to WP-VOL-1): the `native`
//! engine keeps the relative-CasRef law (targets stay under cwd); the bind
//! variants accept absolute targets under ns/vz.

use lightr_core::{LightrError, Result};

// `MountKind` + `ResolvedMount` are DEFINED in `lightr-engine` (the lower crate
// `ExecSpec` lives in; lightr-run depends on lightr-engine, so the types ExecSpec
// borrows must live there to stay acyclic). R-MOUNT names THIS file as the type
// home, so we re-export them here — this module is the single canonical surface.
pub use lightr_engine::{MountKind, ResolvedMount};

/// A mount BEFORE resolution — the direct parse of one `-v` / `--mount` /
/// `--tmpfs` flag. `source` is `None` for anonymous volumes and tmpfs.
/// `opts` carries the raw, unparsed long-form options (e.g. `ro`, `bind`,
/// `size=64m`); WP-VOL-1 interprets them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountSpec {
    pub kind: MountKind,
    pub source: Option<String>,
    pub target: String,
    pub readonly: bool,
    pub opts: Vec<String>,
}

// `ResolvedMount` (post-resolution, what `ExecSpec` carries) is re-exported
// from `lightr-engine` above. WP-VOL-1 fills how each `MountSpec` resolves to
// one (CasRef hydration, host-path canonicalization, named-volume dir
// allocation, tmpfs sizing).

// ---------------------------------------------------------------------------
// Parser ENTRY POINTS — frozen signatures, behaviour is WP-VOL-1's job.
//
// These deliberately do NOT parse yet: they return the WP-VOL-1 placeholder
// error so the surface compiles and is callable, while the real grammar
// (short `-v src:dst:opts`, `--mount type=…,source=…,target=…`, `--tmpfs
// dst:opts`) lands with WP-VOL-1.
// ---------------------------------------------------------------------------

/// Parse a short `-v` / `--volume` flag value. Behaviour: WP-VOL-1.
pub fn parse_v(_value: &str) -> Result<MountSpec> {
    Err(LightrError::InvalidRef("WP-VOL-1".to_string()))
}

/// Parse a long `--mount type=…,source=…,target=…` flag value. Behaviour:
/// WP-VOL-1.
pub fn parse_mount_long(_value: &str) -> Result<MountSpec> {
    Err(LightrError::InvalidRef("WP-VOL-1".to_string()))
}

/// Parse a `--tmpfs dst[:opts]` flag value. Behaviour: WP-VOL-1.
pub fn parse_tmpfs(_value: &str) -> Result<MountSpec> {
    Err(LightrError::InvalidRef("WP-VOL-1".to_string()))
}
