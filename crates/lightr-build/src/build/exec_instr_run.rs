//! SKELETON-FREEZE: `RUN`/`SHELL` instruction bodies, split from `exec_instr.rs`
//! so a WP touching command execution edits only this file. Behavior-preserving
//! (byte-identical logic to the prior single `exec_instr.rs`); re-exported from
//! `exec_instr` so `exec.rs` calls them as `exec_instr::{run,shell}`.
use lightr_core::{LightrError, Result};

use super::{interp_vec, BuildCtx};
use crate::build::parse::CmdForm;
use crate::build::vars::interpolate;

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
pub(in crate::build) fn run(ctx: &mut BuildCtx, form: &CmdForm) -> Result<()> {
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

/// `SHELL ["exe","arg",...]` (WP-DF-09): set the active shell used to wrap
/// subsequent shell-form RUN (and shell-form ENTRYPOINT/CMD) in THIS stage.
/// The tokens are interpolated against the scope (Docker interpolates build
/// vars in SHELL's JSON array). SHELL state is per-stage — reset at FROM.
pub(in crate::build) fn shell(ctx: &mut BuildCtx, shell: &[String]) -> Result<()> {
    *ctx.current_shell = interp_vec(shell, ctx.scope, ctx.escape)?;
    Ok(())
}
