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
use super::memo::{step_key, ContextKey, TempDirGuard};
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

// WP-DF-03 multi-stage stage table: split into a sibling file (godfile cap) and
// re-exported so `super::exec::StageTable` call sites stay IDENTICAL.
#[path = "exec_stage.rs"]
mod stage;
pub(super) use stage::StageTable;

// WP-C: `--target` validation split into a sibling file (godfile cap).
#[path = "target.rs"]
mod target_mod;

/// Execute a Dockerfile build (final stage). Thin wrapper over [`build_target`]
/// with `target = None` — preserves the pre-WP-C signature. See [`build_target`]
/// for the memoization / `${VAR}` / multi-stage / `--platform` contract.
pub fn build(
    context_dir: &Path,
    dockerfile: &Path,
    name: &str,
    engine: lightr_engine::EngineKind,
    store: &Store,
    build_args: &[(String, String)],
) -> Result<BuildReport> {
    build_target(
        context_dir,
        dockerfile,
        name,
        engine,
        store,
        build_args,
        None,
    )
}

/// Execute a Dockerfile build, optionally stopping at a named `--target` stage.
///
/// - RUN steps use the **native engine** (`rootfs: None`); each step has a
///   content-derived memo key (AC hits replay the cached layer, no exec).
/// - `${VAR}` is interpolated against a `VarScope` (env seeded from the base at
///   FROM + ENV; args from ARG/`--build-arg`) BEFORE keying; the key hashes the
///   POST-interpolation text (v2), so differing ENV/ARG never collide.
/// - **Multi-stage (WP-DF-03):** `FROM <base> [AS <name>]` starts a STAGE keyed
///   INDEPENDENTLY (reset fs/scope/shell/workdir/ENV); `COPY --from=<name|index>`
///   pulls a PRIOR stage's output (folded into the key — a changed upstream busts
///   the copy). `COPY --from=<external image>` is OUT OF SCOPE (honest error).
/// - **WP-C `target`** = `docker build --target <stage>`: `Some(name)` (case-
///   insensitive) selects that stage as the OUTPUT and stops the loop once it
///   finishes (deps are all prior stages, already built); unknown name ⇒ honest
///   error. `None` ⇒ the LAST stage, byte-identical to the pre-WP-C build.
/// - **WP-C `--platform`** folds the resolved platform into every step's key (see
///   `step_key`) and validates a requested platform against the base (see
///   `exec_instr::from`).
#[allow(clippy::too_many_arguments)]
pub fn build_target(
    context_dir: &Path,
    dockerfile: &Path,
    name: &str,
    engine: lightr_engine::EngineKind,
    store: &Store,
    build_args: &[(String, String)],
    target: Option<&str>,
) -> Result<BuildReport> {
    use super::args::{overrides_from_pairs, ArgState};
    use super::parse::parse_dockerfile_full;

    // ARG (DF-08): `--build-arg` overrides + scope state (logic in `build::args`).
    let arg_overrides = overrides_from_pairs(build_args);
    let mut arg_state = ArgState::default();

    // WP-DF-IGNORE: read `<context>/.dockerignore` ONCE at build start. The
    // matcher threads into BOTH the memo key (an ignored file is not hashed ⇒
    // adding it never busts the cache) AND the COPY/ADD executor (it is not
    // copied). No `.dockerignore` ⇒ an empty matcher ⇒ byte-identical to before.
    let ignore = super::dockerignore::DockerIgnore::load(context_dir);

    let text = std::fs::read_to_string(dockerfile).map_err(LightrError::Io)?;
    let (directives, steps) = parse_dockerfile_full(&text)?;
    // The Dockerfile `# escape=` directive (default backslash) controls `\$`
    // literal-escape during interpolation, matching the parser's continuation
    // escape char.
    let escape = directives.escape.unwrap_or('\\') == '\\';

    // WP-C: validate `--target <stage>` up front (fail closed); returns the
    // lowercased target or None (logic in the sibling `target` module).
    let target_lc = target_mod::validate_target(target, &steps)?;
    // `total` = steps actually EXECUTED, counted incrementally (with `--target`
    // the loop stops early; without it this equals every step, as before).
    let mut total: u64 = 0;

    let guard = TempDirGuard::new()?;
    let work_dir = &guard.path;

    let mut prev_layer_root: Option<Digest> = None;
    let mut accumulated_env: Vec<(String, String)> = Vec::new();
    let mut current_workdir = String::from("/");
    // Active SHELL for shell-form RUN (WP-DF-09): set by SHELL, reset at FROM,
    // folded into the RUN memo key so a differing SHELL can't false-hit.
    let mut current_shell = exec_instr::default_shell();
    let mut cached_steps: u64 = 0;
    // Interpolation scope: `args` from ARG (via `arg_state`); `env` from the base
    // at FROM + ENV (ENV wins).
    let mut scope = VarScope::default();

    // WP-DF-03 multi-stage state. `stages` records each finished stage's output
    // (index + name) for `COPY --from`; `current_stage_name` is the in-progress
    // `AS <name>`. Each FROM after the first records the prior stage and resets
    // `prev_layer_root` to None so the new stage keys INDEPENDENTLY.
    let mut stages = StageTable::default();
    let mut current_stage_name: Option<String> = None;
    let mut stage_in_progress = false;
    // WP-C: the RESOLVED platform of the stage currently building — the
    // `FROM --platform=<p>` value (normalized) when set, else the host platform.
    // Re-derived at every FROM and folded into EVERY step's memo key, so two
    // builds for different platforms never cross-cache. The default (host)
    // matches the pre-WP behavior.
    let mut active_platform = super::platform::host_platform();

    for step in &steps {
        total += 1;
        // Stage boundary: a FROM that is NOT the first finalizes the prior stage
        // (record its output for `COPY --from`) and resets the per-build-key
        // lineage so the new stage is keyed independently.
        if let Instr::From {
            stage, platform, ..
        } = &step.instr
        {
            if stage_in_progress {
                if let Some(root) = prev_layer_root {
                    stages.record(current_stage_name.as_deref(), root);
                }
                prev_layer_root = None;
                // WP-C: if the stage we just finalized IS the `--target`, the
                // build is complete — stop before starting this next FROM. (This
                // FROM step was counted in `total`; un-count it since it does not
                // run.) The finalized stage's output is recorded above, so the
                // post-loop selection picks it as the report root.
                if let (Some(want), Some(done)) = (&target_lc, &current_stage_name) {
                    if done.eq_ignore_ascii_case(want) {
                        total -= 1;
                        break;
                    }
                }
            }
            current_stage_name = stage.clone();
            stage_in_progress = true;
            // WP-C: re-derive the active platform for the NEW stage. The flag is
            // interpolated (Docker allows `--platform=$TARGETPLATFORM`); absent ⇒
            // host. Validation against the base happens in `exec_instr::from`.
            let plat_flag = match platform {
                Some(p) => Some(interpolate(p, &scope, escape)?),
                None => None,
            };
            active_platform = super::platform::resolve_platform(plat_flag.as_deref());
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
            ContextKey {
                context_dir,
                ignore: &ignore,
            },
            &scope,
            escape,
            &current_shell,
            from_stage_digest,
            &active_platform,
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
            ignore: &ignore,
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

    // The output is the LAST stage that ran (WP-DF-03) — which, with `--target`
    // (WP-C), is the target stage (the loop stopped once it finished). Record the
    // in-progress stage's output so the table is complete (and a `COPY --from=
    // <last>` would resolve).
    if let Some(root) = prev_layer_root {
        if stage_in_progress {
            stages.record(current_stage_name.as_deref(), root);
        }
    }
    // Select the report root. With `--target`, the target stage's recorded output
    // is the result (it was recorded either at its FROM boundary on break, or by
    // the line above when it was the last stage). Without a target, it is the
    // last stage's output (`prev_layer_root`). An empty Dockerfile (no stage ever
    // produced a root) is a fail-closed error.
    let final_root = match &target_lc {
        Some(want) => stages.resolve(want)?,
        None => prev_layer_root
            .ok_or_else(|| LightrError::InvalidManifest("empty Dockerfile".to_string()))?,
    };

    Ok(BuildReport {
        name: name.to_string(),
        root: final_root,
        steps: total,
        cached_steps,
    })
}

// End-to-end test modules — each a sibling file (godfile cap). WP-C (`--target`
// + `FROM --platform`) and the per-WP DF-* e2e suites (05/09/06/07/03/cfg/ignore).
#[cfg(test)]
#[path = "exec_df03_tests.rs"]
mod df03_tests;
#[cfg(test)]
#[path = "exec_df05_tests.rs"]
mod df05_tests;
#[cfg(test)]
#[path = "exec_df06_tests.rs"]
mod df06_tests;
#[cfg(test)]
#[path = "exec_df07_tests.rs"]
mod df07_tests;
#[cfg(test)]
#[path = "exec_df09_tests.rs"]
mod df09_tests;
#[cfg(test)]
#[path = "exec_df_ignore_tests.rs"]
mod df_ignore_tests;
#[cfg(test)]
#[path = "exec_imgcfg_tests.rs"]
mod imgcfg_tests;
#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "exec_wpc_tests.rs"]
mod wpc_tests;
