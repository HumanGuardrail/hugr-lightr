//! Per-instruction execution bodies for the Dockerfile build loop.
//!
//! `build()` (in `build/exec.rs`) is a thin dispatch: it computes the memo key,
//! does the AC lookup, then `match`es on the instruction and calls exactly one
//! of the `pub(super) fn`s here. Each instruction's execution is a SELF-CONTAINED
//! function over a shared `BuildCtx`, so a future WP touching instruction X edits
//! only `fn x` and never collides with a WP touching instruction Y.
//!
//! Behavior-preserving: every body is byte-identical logic to the prior single
//! `match` in `exec.rs`; the memo key (computed by the caller) is unchanged.
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::Path;

use super::args::{ArgOverrides, ArgState};
use super::exec_fs::copy_dir_recursive;
use super::memo::{load_meta, save_meta};
use super::parse::{CmdForm, Instr};
use super::vars::{interpolate, VarScope};

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
}

/// Interpolate every string in a slice against `scope`.
fn interp_vec(v: &[String], scope: &VarScope, escape: bool) -> Result<Vec<String>> {
    v.iter().map(|s| interpolate(s, scope, escape)).collect()
}

/// `FROM`: hydrate the base image into a cleared work dir and (re)seed the
/// interpolation scope from the base config ENV + the stage ARG boundary.
pub(super) fn from(ctx: &mut BuildCtx, instr: &Instr, image_ref: &str) -> Result<()> {
    // FROM ref is interpolated against the GLOBAL ARG scope (Docker:
    // ARG-before-FROM is usable here); multi-stage refs are DF-03.
    let image_ref = interpolate(image_ref, ctx.scope, ctx.escape)?;
    for entry in std::fs::read_dir(ctx.work_dir).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let p = entry.path();
        if p.is_dir() && !p.is_symlink() {
            std::fs::remove_dir_all(&p).map_err(LightrError::Io)?;
        } else {
            std::fs::remove_file(&p).map_err(LightrError::Io)?;
        }
    }
    if image_ref != "scratch" {
        lightr_index::hydrate(ctx.work_dir, ctx.store, &image_ref)?;
    }
    // Seed the interpolation scope from the base image's config ENV.
    // The hydrated base carries lightr's `.lightr-image.json` sidecar
    // (env/cmd/labels) for lightr-built bases; absent (e.g. scratch
    // or an OCI base without the sidecar) → empty, per the design.
    let base = load_meta(ctx.work_dir);
    *ctx.accumulated_env = base.env.clone();
    ctx.scope.env = ctx.accumulated_env.iter().cloned().collect();
    // Stage boundary: global ARGs do NOT cross into the stage (Docker).
    ctx.arg_state
        .sync(instr, ctx.arg_overrides, &mut ctx.scope.args);
    // SHELL is per-stage (WP-DF-09): a new stage resets to the default shell.
    *ctx.current_shell = default_shell();
    Ok(())
}

/// `RUN`: execute the command with the native engine (no rootfs isolation), env
/// from the accumulated ENV, cwd from the current WORKDIR.
///
/// The argv is built from `form` at EXEC time (WP-DF-09), not the parse-baked
/// argv, so the active SHELL applies:
/// - **Exec form** `RUN ["a","b"]` — argv verbatim (SHELL does NOT apply, Docker).
/// - **Shell form** `RUN cmd` — `current_shell ++ [cmd]` (e.g. `/bin/bash -c cmd`
///   after `SHELL ["/bin/bash","-c"]`; default `/bin/sh -c cmd`).
///
/// Every token (the shell prefix's args from interpolation aside — the shell
/// exe/flags come from a parsed JSON array and are used verbatim) of the command
/// is interpolated against the scope as before.
pub(super) fn run(ctx: &mut BuildCtx, form: &CmdForm) -> Result<()> {
    let resolved = match form {
        CmdForm::Exec(v) => interp_vec(v, ctx.scope, ctx.escape)?,
        CmdForm::Shell(s) => {
            let cmd = interpolate(s, ctx.scope, ctx.escape)?;
            let mut argv = ctx.current_shell.clone();
            argv.push(cmd);
            argv
        }
    };
    let argv = &resolved;
    let cwd = if *ctx.current_workdir == "/" || ctx.current_workdir.is_empty() {
        ctx.work_dir.to_path_buf()
    } else {
        let rel = ctx.current_workdir.trim_start_matches('/');
        let cwd = ctx.work_dir.join(rel);
        std::fs::create_dir_all(&cwd).map_err(LightrError::Io)?;
        cwd
    };
    let eng = lightr_engine::engine_for(ctx.engine)?;
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
    for (k, v) in ctx.accumulated_env.iter() {
        cmd_builder.env(k, v);
    }
    let code = eng.run(&spec)?;
    if code != 0 {
        return Err(LightrError::InvalidManifest(format!(
            "RUN step exited with code {code}: {:?}",
            argv
        )));
    }
    Ok(())
}

/// `COPY`: copy interpolated context sources into the work dir.
pub(super) fn copy(ctx: &mut BuildCtx, src: &[String], dest: &str) -> Result<()> {
    // Interpolate COPY paths (+ --chown/--chmod are flag fields not
    // used by this executor yet; DF-04 wires them). Paths into the
    // CONTEXT use the interpolated src; the key already hashed the
    // interpolated text + the content of these resolved sources.
    let src = &interp_vec(src, ctx.scope, ctx.escape)?;
    let dest = &interpolate(dest, ctx.scope, ctx.escape)?;
    let dest_path = if dest.starts_with('/') {
        ctx.work_dir.join(dest.trim_start_matches('/'))
    } else {
        ctx.work_dir.join(dest)
    };
    let dest_is_dir = dest.ends_with('/') || src.len() > 1;
    if dest_is_dir {
        std::fs::create_dir_all(&dest_path).map_err(LightrError::Io)?;
        for s in src {
            let src_path = ctx.context_dir.join(s);
            if src_path.is_file() {
                let file_name = src_path.file_name().unwrap();
                std::fs::copy(&src_path, dest_path.join(file_name)).map_err(LightrError::Io)?;
            } else if src_path.is_dir() {
                copy_dir_recursive(&src_path, &dest_path)?;
            }
        }
    } else {
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
        }
        let src_path = ctx.context_dir.join(&src[0]);
        if src_path.is_file() {
            std::fs::copy(&src_path, &dest_path).map_err(LightrError::Io)?;
        } else if src_path.is_dir() {
            std::fs::create_dir_all(&dest_path).map_err(LightrError::Io)?;
            copy_dir_recursive(&src_path, &dest_path)?;
        }
    }
    Ok(())
}

/// `ENV`: update the scope + accumulated ENV for all pairs, persisting to meta.
pub(super) fn env(ctx: &mut BuildCtx, pairs: &[(String, String)]) -> Result<()> {
    // ENV updates the scope for ALL pairs (WP-DF-05 multi-pair).
    // Each value is interpolated against the scope AS IT EVOLVES
    // left-to-right, so a later pair can reference an earlier one in
    // the SAME instruction (Docker semantics). Keys are NOT
    // interpolated (Docker treats ENV/ARG names literally). A
    // single-pair `ENV K v` updates exactly one key, unchanged.
    for (key, raw_val) in pairs {
        let val = interpolate(raw_val, ctx.scope, ctx.escape)?;
        ctx.accumulated_env.retain(|(k, _)| k != key);
        ctx.accumulated_env.push((key.clone(), val.clone()));
        ctx.scope.env.insert(key.clone(), val);
    }
    let mut meta = load_meta(ctx.work_dir);
    meta.env = ctx.accumulated_env.clone();
    save_meta(ctx.work_dir, &meta)?;
    Ok(())
}

/// `WORKDIR`: set the current workdir + ensure it exists in the work dir.
pub(super) fn workdir(ctx: &mut BuildCtx, path: &str) -> Result<()> {
    let path = interpolate(path, ctx.scope, ctx.escape)?;
    *ctx.current_workdir = path.clone();
    let abs = if path.starts_with('/') {
        ctx.work_dir.join(path.trim_start_matches('/'))
    } else {
        ctx.work_dir.join(&path)
    };
    std::fs::create_dir_all(&abs).map_err(LightrError::Io)?;
    Ok(())
}

/// `CMD`: record the (interpolated) default argv into image meta.
pub(super) fn cmd(ctx: &mut BuildCtx, argv: &[String]) -> Result<()> {
    let argv = interp_vec(argv, ctx.scope, ctx.escape)?;
    let mut meta = load_meta(ctx.work_dir);
    meta.cmd = Some(argv);
    save_meta(ctx.work_dir, &meta)?;
    Ok(())
}

/// `LABEL`: record all (interpolated) pairs into image meta. Labels are not
/// build vars, so they do NOT update the VarScope (Docker semantics).
pub(super) fn label(ctx: &mut BuildCtx, pairs: &[(String, String)]) -> Result<()> {
    let mut meta = load_meta(ctx.work_dir);
    for (key, raw_val) in pairs {
        let val = interpolate(raw_val, ctx.scope, ctx.escape)?;
        meta.labels.retain(|(k, _)| k != key);
        meta.labels.push((key.clone(), val));
    }
    save_meta(ctx.work_dir, &meta)?;
    Ok(())
}

/// `ARG`: resolve + bind into the ARG scope (logic in `build::args`).
pub(super) fn arg(ctx: &mut BuildCtx, instr: &Instr) -> Result<()> {
    ctx.arg_state
        .sync(instr, ctx.arg_overrides, &mut ctx.scope.args);
    Ok(())
}

/// `SHELL ["exe","arg",...]` (WP-DF-09): set the active shell used to wrap
/// subsequent shell-form RUN (and shell-form ENTRYPOINT/CMD) in THIS stage.
/// The tokens are interpolated against the scope (Docker interpolates build
/// vars in SHELL's JSON array). SHELL state is per-stage — reset at FROM.
pub(super) fn shell(ctx: &mut BuildCtx, shell: &[String]) -> Result<()> {
    *ctx.current_shell = interp_vec(shell, ctx.scope, ctx.escape)?;
    Ok(())
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
