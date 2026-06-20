//! Per-instruction execution bodies for the Dockerfile build loop.
//!
//! `build()` (in `build/exec.rs`) is a thin dispatch: it computes the memo key,
//! does the AC lookup, then `match`es on the instruction and calls exactly one
//! `exec_instr::*` body. Each instruction's execution is a SELF-CONTAINED
//! function over a shared `BuildCtx`, so a future WP touching instruction X edits
//! only that body and never collides with a WP touching instruction Y.
//!
//! SKELETON-FREEZE: each instruction GROUP lives in its own sibling file so WPs
//! touching different instructions are FILE-DISJOINT. This file is the shared
//! HUB: it owns `BuildCtx` (the per-step state contract), the cross-group
//! helpers (`interp_vec`, `default_shell`), the fail-closed `unsupported` path,
//! and the `#[path]` mod decls + re-exports that keep every `exec_instr::*` call
//! site in `exec.rs` IDENTICAL. The per-group bodies:
//!   - `exec_instr_from.rs` — FROM/stage              → `from`
//!   - `exec_instr_run.rs`  — RUN/SHELL               → `run`, `shell`
//!   - `exec_instr_copy.rs` — COPY/ADD (+ shared glob)→ `copy`, `add`
//!   - `exec_instr_env.rs`  — ENV/LABEL/ARG           → `env`, `label`, `arg`
//!   - `exec_instr_cfg.rs`  — config records: ENTRYPOINT/USER/EXPOSE/STOPSIGNAL/
//!     VOLUME/WORKDIR/CMD
//!
//! Behavior-preserving: every body is byte-identical logic to the prior single
//! `exec_instr.rs`; the memo key (computed by the caller) is unchanged.
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::Path;

use super::args::{ArgOverrides, ArgState};
use super::exec::StageTable;
use super::imgcfg::ImageConfig;
use super::parse::Instr;
use super::vars::{interpolate, VarScope};

// SKELETON-FREEZE: per-instruction-group bodies live in sibling files and are
// re-exported here so `exec.rs` keeps calling them as `exec_instr::*`. Adding or
// editing an instruction is a self-contained edit in exactly one of these files.
#[path = "exec_instr_from.rs"]
mod from_instr;
pub(super) use from_instr::from;

#[path = "exec_instr_run.rs"]
mod run_instr;
pub(super) use run_instr::{run, shell};

#[path = "exec_instr_copy.rs"]
mod copy_instr;
pub(super) use copy_instr::{add, copy};

#[path = "exec_instr_env.rs"]
mod env_instr;
pub(super) use env_instr::{arg, env, label};

// WP-DF-IMGCFG: pure-metadata config-record bodies (ENTRYPOINT/USER/EXPOSE/
// STOPSIGNAL/VOLUME) + WORKDIR/CMD (consolidated here per skeleton-freeze) live
// in a sibling file (godfile cap), re-exported here.
#[path = "exec_instr_cfg.rs"]
mod cfg;
pub(super) use cfg::{
    cmd, entrypoint, expose, healthcheck, onbuild, stopsignal, user, volume, workdir,
};

/// The default SHELL for shell-form RUN/ENTRYPOINT/CMD (Docker's default
/// `["/bin/sh","-c"]`). SHELL state is per-stage and resets to this at FROM.
pub(super) fn default_shell() -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string()]
}

/// The per-step mutable+immutable state every instruction body reads/writes.
///
/// Immutable for the whole build: `work_dir`, `store`, `context_dir`,
/// `engine`, `escape`, `arg_overrides`. Mutated across steps: `scope`,
/// `arg_state`, `accumulated_env`, `current_workdir`, `current_shell`. The
/// loop-only state (`prev_layer_root`, `cached_steps`, snapshot `name`) stays in
/// `build()` and is not part of the per-instruction contract.
///
/// `current_shell` (WP-DF-09) is the active SHELL for shell-form RUN (and
/// shell-form ENTRYPOINT/CMD): set by the SHELL instruction, consumed by `run`,
/// reset to `default_shell()` at every FROM (SHELL is per-stage in Docker).
pub(super) struct BuildCtx<'a> {
    pub work_dir: &'a Path,
    pub store: &'a Store,
    pub context_dir: &'a Path,
    pub engine: lightr_engine::EngineKind,
    pub escape: bool,
    pub arg_overrides: &'a ArgOverrides,
    pub scope: &'a mut VarScope,
    pub arg_state: &'a mut ArgState,
    pub accumulated_env: &'a mut Vec<(String, String)>,
    pub current_workdir: &'a mut String,
    pub current_shell: &'a mut Vec<String>,
    /// WP-DF-03: the output trees of every PRIOR stage, for `COPY --from=stage`.
    /// Read-only within an instruction body; the loop in `exec.rs` records each
    /// stage's output after it finishes. A single-stage build leaves it empty.
    pub stages: &'a StageTable,
}

/// Interpolate every string in a slice against `scope`. Shared cross-group
/// helper (RUN/COPY/ADD/ENV/CMD/ENTRYPOINT/SHELL all interpolate token lists).
fn interp_vec(v: &[String], scope: &VarScope, escape: bool) -> Result<Vec<String>> {
    v.iter().map(|s| interpolate(s, scope, escape)).collect()
}

/// Not-yet-implemented instructions: route to the SAME fail-closed
/// "unsupported instruction" error path as before (behavior-preserving —
/// these never built). WP-DF-01 parses them; execution is DF-02..15.
pub(super) fn unsupported(instr: &Instr) -> Result<()> {
    Err(LightrError::InvalidManifest(format!(
        "unsupported instruction: {}",
        instr_verb(instr)
    )))
}

/// Verb name for an `Instr`, used only to report not-yet-implemented
/// instructions through the existing "unsupported instruction" error path.
fn instr_verb(instr: &Instr) -> &'static str {
    match instr {
        Instr::From { .. } => "FROM",
        Instr::Run { .. } => "RUN",
        Instr::Cmd { .. } => "CMD",
        Instr::Entrypoint { .. } => "ENTRYPOINT",
        Instr::Label { .. } => "LABEL",
        Instr::Expose { .. } => "EXPOSE",
        Instr::Env { .. } => "ENV",
        Instr::Add { .. } => "ADD",
        Instr::Copy { .. } => "COPY",
        Instr::Volume { .. } => "VOLUME",
        Instr::User { .. } => "USER",
        Instr::Workdir { .. } => "WORKDIR",
        Instr::Arg { .. } => "ARG",
        Instr::Onbuild { .. } => "ONBUILD",
        Instr::Stopsignal { .. } => "STOPSIGNAL",
        Instr::Healthcheck { .. } => "HEALTHCHECK",
        Instr::Shell { .. } => "SHELL",
    }
}
