//! Build execution: the `build()` orchestration loop and `BuildReport`.
//!
//! `build()` is a thin dispatcher: per step it computes the memo key, does the
//! AC lookup, then `match`es the instruction and calls exactly ONE
//! self-contained `exec_instr::*` function. The per-instruction execution bodies
//! live in the sibling `build/exec_instr.rs` (one `fn` per instruction, so WPs
//! touching different instructions stay disjoint). Filesystem/CAS helpers
//! (`materialize_from_digest`, `copy_dir_recursive`, `step_reads_clock_or_net`)
//! live in `build/exec_fs.rs`.
use lightr_core::{Digest, LightrError, Result};
use lightr_store::Store;
use std::path::Path;

use super::exec_fs::materialize_from_digest;
// Re-imported for the test module (`super::*`), which exercises the heuristic.
#[cfg(test)]
use super::exec_fs::step_reads_clock_or_net;
use super::exec_instr::{self, BuildCtx};
use super::memo::{load_meta, step_key, TempDirGuard};
use super::parse::Instr;
use super::vars::{interpolate, VarScope};

pub struct BuildReport {
    pub name: String,
    pub root: Digest,
    pub steps: u64,
    pub cached_steps: u64,
}
/// Execute a Dockerfile build.
///
/// - RUN steps use the **native engine** (`rootfs: None`); no filesystem
///   isolation. Memoization: each step has a content-derived key; AC hits
///   replay the cached layer without executing.
/// - Build-time `${VAR}` interpolation (WP-DF-BUILDKEY): each instruction's text
///   is interpolated against a `VarScope` BEFORE executing/keying. `env` is
///   seeded from the base image (after FROM) + updated by ENV; `args` by ARG
///   (DF-08, `build_args` = `--build-arg`). The memo key hashes the
///   POST-INTERPOLATION text (v2) — differing ENV/ARG never collide on a stale
///   layer; an UNUSED ARG changes no text, so it never busts the cache.
pub fn build(
    context_dir: &Path,
    dockerfile: &Path,
    name: &str,
    engine: lightr_engine::EngineKind,
    store: &Store,
    build_args: &[(String, String)],
) -> Result<BuildReport> {
    use super::args::{overrides_from_pairs, ArgState};
    use super::parse::parse_dockerfile_full;

    // ARG (DF-08): `--build-arg` overrides + scope state (logic in `build::args`).
    let arg_overrides = overrides_from_pairs(build_args);
    let mut arg_state = ArgState::default();

    let text = std::fs::read_to_string(dockerfile).map_err(LightrError::Io)?;
    let (directives, steps) = parse_dockerfile_full(&text)?;
    // The Dockerfile `# escape=` directive (default backslash) controls `\$`
    // literal-escape during interpolation, matching the parser's continuation
    // escape char.
    let escape = directives.escape.unwrap_or('\\') == '\\';
    let total = steps.len() as u64;

    let guard = TempDirGuard::new()?;
    let work_dir = &guard.path;

    let mut prev_layer_root: Option<Digest> = None;
    let mut accumulated_env: Vec<(String, String)> = Vec::new();
    let mut current_workdir = String::from("/");
    // Active SHELL for shell-form RUN (WP-DF-09): default `["/bin/sh","-c"]`,
    // set by SHELL, reset at every FROM (per-stage). Folded into the RUN memo
    // key (step_key) so a differing SHELL can never false-hit a cached layer.
    let mut current_shell = exec_instr::default_shell();
    let mut cached_steps: u64 = 0;
    // Interpolation scope: `args` seeded by ARG (DF-08, via `arg_state`); `env`
    // seeded from the base after FROM + updated by ENV (ENV wins over ARG).
    let mut scope = VarScope::default();

    for step in &steps {
        let key = step_key(
            prev_layer_root,
            step,
            context_dir,
            &scope,
            escape,
            &current_shell,
        )?;

        // AC lookup
        if let Some(cached_val) = store.ac_get(&key)? {
            if cached_val.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&cached_val);
                let cached_root = Digest(arr);
                materialize_from_digest(work_dir, &cached_root, store)?;
                prev_layer_root = Some(cached_root);
                cached_steps += 1;
                let meta = load_meta(work_dir);
                accumulated_env = meta.env.clone();
                // Keep the interpolation scope in sync with the replayed layer's
                // accumulated ENV (so subsequent steps interpolate correctly even
                // when earlier ENV/FROM steps were cache hits).
                scope.env = accumulated_env.iter().cloned().collect();
                // Re-derive ARG/FROM scope on the cache-hit path too (not in meta).
                arg_state.sync(&step.instr, &arg_overrides, &mut scope.args);
                if let Instr::Workdir { path } = &step.instr {
                    current_workdir = interpolate(path, &scope, escape)?;
                }
                // Re-derive the active SHELL on the cache-hit path (WP-DF-09):
                // FROM resets it (per-stage); SHELL sets it. This keeps a later
                // RUN's key correct even when the SHELL/FROM step was a cache hit.
                match &step.instr {
                    Instr::From { .. } => current_shell = exec_instr::default_shell(),
                    Instr::Shell { shell } => {
                        current_shell = shell
                            .iter()
                            .map(|s| interpolate(s, &scope, escape))
                            .collect::<Result<Vec<_>>>()?;
                    }
                    _ => {}
                }
                continue;
            }
        }

        // Thin dispatch: each arm calls exactly one self-contained
        // `exec_instr::*` body over the shared `BuildCtx` (behavior-preserving;
        // adding/editing an instruction is a single-`fn` edit there).
        let mut ctx = BuildCtx {
            work_dir,
            store,
            context_dir,
            engine,
            escape,
            arg_overrides: &arg_overrides,
            scope: &mut scope,
            arg_state: &mut arg_state,
            accumulated_env: &mut accumulated_env,
            current_workdir: &mut current_workdir,
            current_shell: &mut current_shell,
        };
        match &step.instr {
            Instr::From { image_ref, .. } => exec_instr::from(&mut ctx, &step.instr, image_ref)?,
            // RUN consumes the structured `form` (WP-DF-09): shell form is wrapped
            // by the active SHELL at exec time, not the parse-baked `/bin/sh -c`.
            Instr::Run { form, .. } => exec_instr::run(&mut ctx, form)?,
            // WP-DF-06: COPY wires --from/--chown/--chmod through to the executor
            // (--from is an honest "unsupported until DF-03" error there).
            Instr::Copy {
                src,
                dest,
                from,
                chown,
                chmod,
            } => exec_instr::copy(
                &mut ctx,
                src,
                dest,
                from.as_deref(),
                chown.as_deref(),
                chmod.as_deref(),
            )?,
            // WP-DF-07: ADD = COPY local semantics + tar auto-extract; a URL src
            // is an honest "non-hermetic, unsupported" error in the executor.
            Instr::Add {
                src,
                dest,
                chown,
                chmod,
            } => exec_instr::add(&mut ctx, src, dest, chown.as_deref(), chmod.as_deref())?,
            Instr::Env { pairs } => exec_instr::env(&mut ctx, pairs)?,
            Instr::Workdir { path } => exec_instr::workdir(&mut ctx, path)?,
            Instr::Cmd { argv, .. } => exec_instr::cmd(&mut ctx, argv)?,
            Instr::Label { pairs } => exec_instr::label(&mut ctx, pairs)?,
            Instr::Arg { .. } => exec_instr::arg(&mut ctx, &step.instr)?,
            Instr::Shell { shell } => exec_instr::shell(&mut ctx, shell)?,
            // WP-DF-01 parses these into the AST; execution is DF-02..15. Until
            // then they route to the SAME "unsupported instruction" error path
            // as before (fail-closed, behavior-preserving — these never built).
            other => exec_instr::unsupported(other)?,
        }

        let snap = lightr_index::snapshot(work_dir, store, name)?;
        let new_root = snap.root;
        store.ac_put(&key, &new_root.0)?;
        prev_layer_root = Some(new_root);
    }

    let final_root = prev_layer_root
        .ok_or_else(|| LightrError::InvalidManifest("empty Dockerfile".to_string()))?;

    Ok(BuildReport {
        name: name.to_string(),
        root: final_root,
        steps: total,
        cached_steps,
    })
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

// WP-DF-05 end-to-end tests live in a sibling file to keep each under the
// 400-line godfile cap.
#[cfg(test)]
#[path = "exec_df05_tests.rs"]
mod df05_tests;

// WP-DF-09 SHELL end-to-end tests (sibling file, godfile cap).
#[cfg(test)]
#[path = "exec_df09_tests.rs"]
mod df09_tests;

// WP-DF-06 COPY parity end-to-end tests (sibling file, godfile cap).
#[cfg(test)]
#[path = "exec_df06_tests.rs"]
mod df06_tests;

// WP-DF-07 ADD end-to-end tests: local copy (reuses DF-06) + tar auto-extract +
// URL honest-unsupported + memo no-false-hit (sibling file, godfile cap).
#[cfg(test)]
#[path = "exec_df07_tests.rs"]
mod df07_tests;
