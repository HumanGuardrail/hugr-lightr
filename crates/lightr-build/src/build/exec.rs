//! Build execution: the `build()` orchestration loop, `BuildReport`, and the
//! per-instruction execution arms. Filesystem/CAS helpers
//! (`materialize_from_digest`, `copy_dir_recursive`, `step_reads_clock_or_net`)
//! live in the sibling `build/exec_fs.rs`.
use lightr_core::{Digest, LightrError, Result};
use lightr_store::Store;
use std::path::Path;

use super::exec_fs::{copy_dir_recursive, materialize_from_digest};
// Re-imported for the test module (`super::*`), which exercises the heuristic.
#[cfg(test)]
use super::exec_fs::step_reads_clock_or_net;
use super::memo::{load_meta, save_meta, step_key, TempDirGuard};
use super::parse::Instr;
use super::vars::{interpolate, VarScope};

/// Interpolate every string in a slice against `scope`.
fn interp_vec(v: &[String], scope: &VarScope, escape: bool) -> Result<Vec<String>> {
    v.iter().map(|s| interpolate(s, scope, escape)).collect()
}

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
    let mut cached_steps: u64 = 0;
    // Interpolation scope: `args` seeded by ARG (DF-08, via `arg_state`); `env`
    // seeded from the base after FROM + updated by ENV (ENV wins over ARG).
    let mut scope = VarScope::default();

    for step in &steps {
        let key = step_key(prev_layer_root, step, context_dir, &scope, escape)?;

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
                continue;
            }
        }

        match &step.instr {
            Instr::From { image_ref, .. } => {
                // FROM ref is interpolated against the GLOBAL ARG scope (Docker:
                // ARG-before-FROM is usable here); multi-stage refs are DF-03.
                let image_ref = interpolate(image_ref, &scope, escape)?;
                for entry in std::fs::read_dir(work_dir).map_err(LightrError::Io)? {
                    let entry = entry.map_err(LightrError::Io)?;
                    let p = entry.path();
                    if p.is_dir() && !p.is_symlink() {
                        std::fs::remove_dir_all(&p).map_err(LightrError::Io)?;
                    } else {
                        std::fs::remove_file(&p).map_err(LightrError::Io)?;
                    }
                }
                if image_ref != "scratch" {
                    lightr_index::hydrate(work_dir, store, &image_ref)?;
                }
                // Seed the interpolation scope from the base image's config ENV.
                // The hydrated base carries lightr's `.lightr-image.json` sidecar
                // (env/cmd/labels) for lightr-built bases; absent (e.g. scratch
                // or an OCI base without the sidecar) → empty, per the design.
                let base = load_meta(work_dir);
                accumulated_env = base.env.clone();
                scope.env = accumulated_env.iter().cloned().collect();
                // Stage boundary: global ARGs do NOT cross into the stage (Docker).
                arg_state.sync(&step.instr, &arg_overrides, &mut scope.args);
            }
            Instr::Run { argv, .. } => {
                let argv = &interp_vec(argv, &scope, escape)?;
                let cwd = if current_workdir == "/" || current_workdir.is_empty() {
                    work_dir.to_path_buf()
                } else {
                    let rel = current_workdir.trim_start_matches('/');
                    let cwd = work_dir.join(rel);
                    std::fs::create_dir_all(&cwd).map_err(LightrError::Io)?;
                    cwd
                };
                let eng = lightr_engine::engine_for(engine)?;
                let spec = lightr_engine::ExecSpec {
                    cwd: &cwd,
                    command: argv,
                    rootfs: None,
                    limits: Default::default(),
                    net: false,
                    net_fd: None,
                    net_mac: None,
                    mounts: &[],
                    env: &[],
                    workdir: None,
                    user: None,
                    hostname: None,
                    add_host: &[],
                    dns: &[],
                    mesh_ip: None,
                };
                let mut cmd_builder = std::process::Command::new(&argv[0]);
                if argv.len() > 1 {
                    cmd_builder.args(&argv[1..]);
                }
                for (k, v) in &accumulated_env {
                    cmd_builder.env(k, v);
                }
                let code = eng.run(&spec)?;
                if code != 0 {
                    return Err(LightrError::InvalidManifest(format!(
                        "RUN step exited with code {code}: {:?}",
                        argv
                    )));
                }
            }
            Instr::Copy { src, dest, .. } => {
                // Interpolate COPY paths (+ --chown/--chmod are flag fields not
                // used by this executor yet; DF-04 wires them). Paths into the
                // CONTEXT use the interpolated src; the key already hashed the
                // interpolated text + the content of these resolved sources.
                let src = &interp_vec(src, &scope, escape)?;
                let dest = &interpolate(dest, &scope, escape)?;
                let dest_path = if dest.starts_with('/') {
                    work_dir.join(dest.trim_start_matches('/'))
                } else {
                    work_dir.join(dest)
                };
                let dest_is_dir = dest.ends_with('/') || src.len() > 1;
                if dest_is_dir {
                    std::fs::create_dir_all(&dest_path).map_err(LightrError::Io)?;
                    for s in src {
                        let src_path = context_dir.join(s);
                        if src_path.is_file() {
                            let file_name = src_path.file_name().unwrap();
                            std::fs::copy(&src_path, dest_path.join(file_name))
                                .map_err(LightrError::Io)?;
                        } else if src_path.is_dir() {
                            copy_dir_recursive(&src_path, &dest_path)?;
                        }
                    }
                } else {
                    if let Some(parent) = dest_path.parent() {
                        std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
                    }
                    let src_path = context_dir.join(&src[0]);
                    if src_path.is_file() {
                        std::fs::copy(&src_path, &dest_path).map_err(LightrError::Io)?;
                    } else if src_path.is_dir() {
                        std::fs::create_dir_all(&dest_path).map_err(LightrError::Io)?;
                        copy_dir_recursive(&src_path, &dest_path)?;
                    }
                }
            }
            Instr::Env { pairs } => {
                // ENV updates the scope for ALL pairs (WP-DF-05 multi-pair).
                // Each value is interpolated against the scope AS IT EVOLVES
                // left-to-right, so a later pair can reference an earlier one in
                // the SAME instruction (Docker semantics). Keys are NOT
                // interpolated (Docker treats ENV/ARG names literally). A
                // single-pair `ENV K v` updates exactly one key, unchanged.
                for (key, raw_val) in pairs {
                    let val = interpolate(raw_val, &scope, escape)?;
                    accumulated_env.retain(|(k, _)| k != key);
                    accumulated_env.push((key.clone(), val.clone()));
                    scope.env.insert(key.clone(), val);
                }
                let mut meta = load_meta(work_dir);
                meta.env = accumulated_env.clone();
                save_meta(work_dir, &meta)?;
            }
            Instr::Workdir { path } => {
                let path = interpolate(path, &scope, escape)?;
                current_workdir = path.clone();
                let abs = if path.starts_with('/') {
                    work_dir.join(path.trim_start_matches('/'))
                } else {
                    work_dir.join(&path)
                };
                std::fs::create_dir_all(&abs).map_err(LightrError::Io)?;
            }
            Instr::Cmd { argv, .. } => {
                let argv = interp_vec(argv, &scope, escape)?;
                let mut meta = load_meta(work_dir);
                meta.cmd = Some(argv);
                save_meta(work_dir, &meta)?;
            }
            Instr::Label { pairs } => {
                // LABEL records ALL pairs (WP-DF-05 multi-pair) into image
                // metadata. Values are interpolated; labels are NOT build vars,
                // so they do NOT update the VarScope (Docker semantics).
                let mut meta = load_meta(work_dir);
                for (key, raw_val) in pairs {
                    let val = interpolate(raw_val, &scope, escape)?;
                    meta.labels.retain(|(k, _)| k != key);
                    meta.labels.push((key.clone(), val));
                }
                save_meta(work_dir, &meta)?;
            }
            Instr::Arg { .. } => {
                // ARG (DF-08): resolve + bind into the ARG scope (logic in `build::args`).
                arg_state.sync(&step.instr, &arg_overrides, &mut scope.args);
            }
            // WP-DF-01 parses these into the AST; execution is DF-02..15. Until
            // then they route to the SAME "unsupported instruction" error path
            // as before (fail-closed, behavior-preserving — these never built).
            other => {
                return Err(LightrError::InvalidManifest(format!(
                    "unsupported instruction: {}",
                    instr_verb(other)
                )));
            }
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

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

// WP-DF-05 end-to-end tests live in a sibling file to keep each under the
// 400-line godfile cap.
#[cfg(test)]
#[path = "exec_df05_tests.rs"]
mod df05_tests;
