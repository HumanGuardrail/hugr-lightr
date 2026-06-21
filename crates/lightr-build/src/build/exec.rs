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
use super::imgcfg::ImageConfig;
use super::memo::{step_key, TempDirGuard};
use super::parse::Instr;
use super::vars::{interpolate, VarScope};

pub struct BuildReport {
    pub name: String,
    pub root: Digest,
    pub steps: u64,
    pub cached_steps: u64,
}

/// Strip the leading `ONBUILD` keyword from an ONBUILD step's raw
/// (continuation-joined) source text, returning the trigger instruction
/// VERBATIM (WP-DF-HEALTHCHECK-ONBUILD). The parser already proved the keyword
/// is present and the trigger is a valid, allowed instruction; this keeps the
/// trigger byte-faithful (no re-serialization) for recording into the config.
fn onbuild_trigger(raw: &str) -> &str {
    let t = raw.trim_start();
    // The first whitespace-delimited token is the `ONBUILD` keyword (any case).
    match t.split_once(|c: char| c.is_ascii_whitespace()) {
        Some((kw, rest)) if kw.eq_ignore_ascii_case("ONBUILD") => rest.trim_start(),
        _ => t.trim_end(),
    }
}

/// The multi-stage stage table (WP-DF-03): the resolved output of every stage
/// that has already finished building, in build order. A `FROM <base> [AS name]`
/// starts a stage; when it completes, its result tree's manifest [`Digest`] is
/// recorded here under its 0-based index AND (when named) its lowercased name.
/// `COPY --from=<name|index>` resolves against this table — so a stage can only
/// reference a PRIOR stage (forward/self refs are absent ⇒ honest error).
#[derive(Default)]
pub(super) struct StageTable {
    /// Each finished stage's output digest, in build order (index = position).
    by_index: Vec<Digest>,
    /// `name → output digest` for `FROM ... AS <name>` stages (lowercased;
    /// Docker matches stage names case-insensitively).
    by_name: std::collections::HashMap<String, Digest>,
}

impl StageTable {
    /// Record a finished stage's output digest at its build-order index, and
    /// (if the stage was named `AS <name>`) under its lowercased name.
    pub(super) fn record(&mut self, name: Option<&str>, digest: Digest) {
        self.by_index.push(digest);
        if let Some(n) = name {
            self.by_name.insert(n.to_ascii_lowercase(), digest);
        }
    }

    /// Resolve a `COPY --from=<ref>` to a PRIOR stage's output digest. `ref` is a
    /// stage NAME (case-insensitive) or a 0-based INDEX (a purely-numeric ref is
    /// an index; otherwise a name). Unknown name, out-of-range / self / forward
    /// index → honest fail-closed error (no silent half-copy). The external-IMAGE
    /// `--from=<image>` form is OUT OF SCOPE for this WP: such a ref is neither a
    /// known stage name nor a valid prior index, so it surfaces the same honest
    /// "unknown stage / external image out of scope" error.
    pub(super) fn resolve(&self, from: &str) -> Result<Digest> {
        if !from.is_empty() && from.chars().all(|c| c.is_ascii_digit()) {
            let idx: usize = from.parse().map_err(|_| {
                LightrError::InvalidManifest(format!("COPY --from: invalid stage index {from:?}"))
            })?;
            return self.by_index.get(idx).copied().ok_or_else(|| {
                LightrError::InvalidManifest(format!(
                    "COPY --from={from}: no such prior stage (index out of range — \
                     forward/self references are not allowed)"
                ))
            });
        }
        self.by_name
            .get(&from.to_ascii_lowercase())
            .copied()
            .ok_or_else(|| {
                LightrError::InvalidManifest(format!(
                    "COPY --from={from}: unknown stage name (only PRIOR named stages are \
                     valid; copying --from an external image is out of scope)"
                ))
            })
    }
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
/// - **Multi-stage (WP-DF-03):** `FROM <base> [AS <name>]` starts a new STAGE; a
///   Dockerfile has 1..N stages. Each stage builds in order with its OWN reset
///   filesystem/scope/shell/workdir/ENV (per-stage at FROM; global pre-FROM ARGs
///   carry through `arg_state`) and is keyed INDEPENDENTLY (lineage resets at the
///   stage boundary). The build OUTPUT is the LAST stage. `COPY --from=<name|
///   index>` copies from a PRIOR stage's resolved output tree (folded into that
///   step's key — a changed upstream stage busts the copy, no false hit).
///   A single-FROM Dockerfile builds BYTE-IDENTICALLY to the pre-WP loop.
///   **Ambiguity / out-of-scope (transcribe, don't design):** (1) `--target
///   <stage>` is NOT wired — the `lightr build` CLI exposes no such flag and the
///   handler is outside this WP's owned files, so the output stays the LAST stage;
///   the stage table is ready for `--target` the moment the CLI grows the flag.
///   (2) `COPY --from=<external image>` is OUT OF SCOPE — only STAGE refs resolve;
///   an external-image `--from` is an honest "unknown stage / out of scope" error.
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

    // WP-DF-03 multi-stage state. `stages` accumulates each finished stage's
    // output (index + name) for `COPY --from`; `current_stage_name` is the
    // in-progress stage's `AS <name>` (None until the first FROM, or an
    // un-named stage). At each FROM AFTER the first, the in-progress stage is
    // recorded into `stages` and `prev_layer_root` is RESET to None — a new
    // stage keys INDEPENDENTLY of prior stages (Docker: stages share nothing
    // but the explicit `COPY --from`). A single-FROM build records exactly one
    // stage at the end and is byte-identical to the pre-WP single-stage loop.
    let mut stages = StageTable::default();
    let mut current_stage_name: Option<String> = None;
    let mut stage_in_progress = false;

    for step in &steps {
        // Stage boundary: a FROM that is NOT the first finalizes the prior stage
        // (record its output for `COPY --from`) and resets the per-build-key
        // lineage so the new stage is keyed independently.
        if let Instr::From { stage, .. } = &step.instr {
            if stage_in_progress {
                if let Some(root) = prev_layer_root {
                    stages.record(current_stage_name.as_deref(), root);
                }
                prev_layer_root = None;
            }
            current_stage_name = stage.clone();
            stage_in_progress = true;
        }

        // WP-DF-03: a `COPY --from=<stage>` folds the SOURCE stage's resolved
        // output digest into its key (no false hit when the upstream changes).
        // Resolved here (fail-closed on unknown/forward/self/external-image) so
        // the AC lookup below already reflects the upstream identity. Non-`--from`
        // steps resolve to None ⇒ key byte-identical to before this WP.
        let from_stage_digest = match &step.instr {
            Instr::Copy {
                from: Some(from), ..
            } => Some(stages.resolve(&interpolate(from, &scope, escape)?)?),
            _ => None,
        };

        let key = step_key(
            prev_layer_root,
            step,
            context_dir,
            &scope,
            escape,
            &current_shell,
            from_stage_digest,
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
                let cfg = ImageConfig::load(work_dir);
                accumulated_env = cfg.env.clone();
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
            stages: &stages,
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
            // WP-DF-IMGCFG: the config instructions now RECORD into the image
            // config sidecar (was the fail-closed "unsupported" path). Each is a
            // pure metadata write (no filesystem mutation), so a layer snapshot
            // still follows below — the sidecar IS the layer's recorded config.
            Instr::Entrypoint { argv, .. } => exec_instr::entrypoint(&mut ctx, argv)?,
            Instr::User { user } => exec_instr::user(&mut ctx, user)?,
            Instr::Expose { ports } => exec_instr::expose(&mut ctx, ports)?,
            Instr::Stopsignal { signal } => exec_instr::stopsignal(&mut ctx, signal)?,
            Instr::Volume { paths } => exec_instr::volume(&mut ctx, paths)?,
            Instr::Label { pairs } => exec_instr::label(&mut ctx, pairs)?,
            Instr::Arg { .. } => exec_instr::arg(&mut ctx, &step.instr)?,
            Instr::Shell { shell } => exec_instr::shell(&mut ctx, shell)?,
            // WP-DF-HEALTHCHECK-ONBUILD: record the OCI healthcheck shape into the
            // image config (incl `NONE` → disabled). Was the "unsupported" path.
            Instr::Healthcheck { check } => exec_instr::healthcheck(&mut ctx, check)?,
            // WP-DF-HEALTHCHECK-ONBUILD: record the ONBUILD trigger VERBATIM (the
            // trigger fires on a DERIVED build that uses this image as a base —
            // trigger-execution is a flagged follow-up; recording is the scope).
            // The trigger text is `step.raw` with the leading `ONBUILD` keyword
            // stripped (the continuation-joined source the parser preserved).
            Instr::Onbuild { .. } => exec_instr::onbuild(&mut ctx, onbuild_trigger(&step.raw))?,
            // All Dockerfile instructions are now implemented — the match is
            // exhaustive (a new Instr variant fails to compile here until handled).
        }

        let snap = lightr_index::snapshot(work_dir, store, name)?;
        let new_root = snap.root;
        store.ac_put(&key, &new_root.0)?;
        prev_layer_root = Some(new_root);
    }

    // The LAST stage's output is the build result (WP-DF-03; `--target` would
    // select a different stage — see the note below: no CLI flag exists yet).
    // Record it too so the table is complete (and a `COPY --from=<last>` from a
    // hypothetical later step would resolve — though none can follow the end).
    let final_root = prev_layer_root
        .ok_or_else(|| LightrError::InvalidManifest("empty Dockerfile".to_string()))?;
    if stage_in_progress {
        stages.record(current_stage_name.as_deref(), final_root);
    }

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

// WP-DF-03 multi-stage end-to-end tests: 2-stage build + COPY --from=name/index +
// unknown/forward/external honest errors + memo no-false-hit + single-stage
// byte-identical (sibling file, godfile cap).
#[cfg(test)]
#[path = "exec_df03_tests.rs"]
mod df03_tests;

// WP-DF-IMGCFG record-side tests: config instructions land in the image config
// sidecar; config-less images keep the default config (sibling file, godfile cap).
#[cfg(test)]
#[path = "exec_imgcfg_tests.rs"]
mod imgcfg_tests;
