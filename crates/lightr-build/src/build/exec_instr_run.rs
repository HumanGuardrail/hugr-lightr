//! SKELETON-FREEZE: `RUN`/`SHELL` instruction bodies, split from `exec_instr.rs`
//! so a WP touching command execution edits only this file. Behavior-preserving
//! (byte-identical logic to the prior single `exec_instr.rs`); re-exported from
//! `exec_instr` so `exec.rs` calls them as `exec_instr::{run,shell}`.
use lightr_core::{LightrError, Result};

use super::{interp_vec, BuildCtx};
use crate::build::parse::CmdForm;
use crate::build::vars::interpolate;

/// `RUN`: execute the command with the native engine (no rootfs isolation), env
/// from the accumulated ENV (+ declared ARGs), cwd from the current WORKDIR.
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
///
/// WP-RUNENV — the child PROCESS environment carries the accumulated build ENV
/// (every `ENV` set before this RUN, in order; later overrides earlier) PLUS the
/// declared ARGs, matching Docker: `ENV X=1` then `RUN printenv X` → `1`, even
/// with no `${X}` in the RUN text (interpolation is separate and already works).
/// Precedence: ARG values seed the env, then ENV overlays them (ENV wins). A RUN
/// with no prior ENV/ARG is byte-identical to before (empty env ⇒ engine no-op).
///
/// KNOWN PARITY GAP (R-KEY is FROZEN, untouched here): the build memo key hashes
/// the POST-INTERPOLATION RUN text, so a RUN that READS an ENV var it does not
/// textually reference (`RUN printenv X`) will NOT bust the cache when only the
/// ENV value changes, where Docker would. Closing that is a separate WP.
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
    // WP-RUNENV: the RUN child's process env = declared ARGs (seeded first) then
    // the accumulated build ENV overlaid on top (ENV wins over ARG; within ENV,
    // later overrides earlier — `accumulated_env` is already ordered+deduped that
    // way by the ENV instruction). The engine overlays `spec.env` onto the
    // inherited parent env, so an empty `run_env` is a no-op (no prior ENV/ARG ⇒
    // byte-identical to before).
    let run_env = build_run_env(ctx);
    let eng = lightr_engine::engine_for(ctx.engine)?;
    let spec = lightr_engine::ExecSpec {
        cwd: &cwd,
        command: argv,
        rootfs: None,
        limits: Default::default(),
        net: false,
        net_isolate: false,
        net_fd: None,
        net_mac: None,
        mounts: &[],
        env: &run_env,
        workdir: None,
        user: None,
        hostname: None,
        add_host: &[],
        dns: &[],
        mesh_ip: None,
        // WP-#92: a build RUN step is never read-only / shm-sized (the build needs a
        // writable rootfs); defaults preserve today's behaviour.
        read_only: false,
        shm_size: None,
        // WP-#94: a build RUN step never drops/adds caps (defaults preserve today's
        // behaviour — the build child keeps the full userns capability set).
        cap_drop: &[],
        cap_add: &[],
        init: false,
        // WP-#99: CRI-only carry-slots; a build RUN step neither joins a netns nor
        // names a cgroup leaf. Defaults preserve today's behaviour.
        join_netns: None,
        cgroup_name: None,
        // WP-#102: a build RUN step is synchronous; no exec-readiness pipe. None.
        exec_ready_fd: None,
        // WP-#106: a build RUN step applies no AppArmor profile. None.
        apparmor: None,
        // WP-#108: a build RUN step applies no seccomp profile. None.
        seccomp: None,
        // WP-#107: no CRI volume mounts / DNS / hostname here.
        bind_mounts: &[],
        resolv_conf: None,
    };
    let code = eng.run(&spec)?;
    if code != 0 {
        return Err(LightrError::InvalidManifest(format!(
            "RUN step exited with code {code}: {:?}",
            argv
        )));
    }
    Ok(())
}

/// Build the RUN child's process environment (WP-RUNENV): declared ARGs first,
/// then the accumulated build ENV overlaid on top so ENV wins over ARG (Docker
/// precedence), and within ENV the existing order is preserved (later overrides
/// earlier — `accumulated_env` is already maintained that way). The engine
/// overlays this onto the inherited parent env via `Command::envs`, where a
/// later pair for the same key wins — hence ARGs are emitted before ENV. An
/// empty result is a no-op (no prior ENV/ARG ⇒ behavior-preserving).
fn build_run_env(ctx: &BuildCtx) -> Vec<(String, String)> {
    let args: Vec<(String, String)> = ctx
        .scope
        .args
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    merge_run_env(&args, ctx.accumulated_env)
}

/// Pure core of `build_run_env` (parallel-safe, no `BuildCtx`): ARGs first, then
/// the accumulated ENV overlaid so ENV wins under `Command::envs` last-key-wins.
fn merge_run_env(
    args: &[(String, String)],
    accumulated_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = args.to_vec();
    env.extend(accumulated_env.iter().cloned());
    env
}

/// `SHELL ["exe","arg",...]` (WP-DF-09): set the active shell used to wrap
/// subsequent shell-form RUN (and shell-form ENTRYPOINT/CMD) in THIS stage.
/// The tokens are interpolated against the scope (Docker interpolates build
/// vars in SHELL's JSON array). SHELL state is per-stage — reset at FROM.
pub(in crate::build) fn shell(ctx: &mut BuildCtx, shell: &[String]) -> Result<()> {
    *ctx.current_shell = interp_vec(shell, ctx.scope, ctx.escape)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::merge_run_env;

    fn pair(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    /// `ENV X=1` ⇒ the RUN env carries `X=1` (the core WP-RUNENV behavior).
    #[test]
    fn env_is_present_in_run_env() {
        let env = merge_run_env(&[], &[pair("X", "1")]);
        assert!(env.iter().any(|(k, v)| k == "X" && v == "1"));
    }

    /// No prior ENV/ARG ⇒ empty env (engine treats empty as a no-op ⇒ the child
    /// inherits exactly the parent env, byte-identical to before WP-RUNENV).
    #[test]
    fn no_env_no_arg_is_empty() {
        assert!(merge_run_env(&[], &[]).is_empty());
    }

    /// A later ENV overrides an earlier one. `accumulated_env` keeps both pairs
    /// in order (the ENV instruction dedups, but be robust): the LAST occurrence
    /// wins under `Command::envs`, so it must be emitted last.
    #[test]
    fn later_env_overrides_earlier() {
        let acc = vec![pair("X", "old"), pair("X", "new")];
        let env = merge_run_env(&[], &acc);
        let last = env.iter().rfind(|(k, _)| k == "X").unwrap();
        assert_eq!(last.1, "new");
    }

    /// ENV wins over ARG of the same name (Docker precedence): the ENV pair is
    /// emitted AFTER the ARG pair, so it is the last-key-wins value.
    #[test]
    fn env_wins_over_arg() {
        let args = vec![pair("V", "from_arg")];
        let env = merge_run_env(&args, &[pair("V", "from_env")]);
        let last = env.iter().rfind(|(k, _)| k == "V").unwrap();
        assert_eq!(last.1, "from_env");
    }

    /// Declared ARGs (without a colliding ENV) reach the RUN env too.
    #[test]
    fn arg_is_present_when_no_env_collision() {
        let args = vec![pair("A", "av")];
        let env = merge_run_env(&args, &[pair("X", "1")]);
        assert!(env.iter().any(|(k, v)| k == "A" && v == "av"));
        assert!(env.iter().any(|(k, v)| k == "X" && v == "1"));
    }

    /// End-to-end proof that a real child PROCESS sees the merged env, using the
    /// same `Command::envs` overlay the native engine applies (last key wins).
    /// `sh -c 'echo $X'` reads the var the RUN text never references textually.
    /// Parallel-safe: spawns its own child, never touches process-global state.
    #[cfg(unix)]
    #[test]
    fn real_child_sees_merged_env_with_env_overriding_arg() {
        let args = vec![pair("X", "from_arg")];
        let env = merge_run_env(&args, &[pair("X", "from_env"), pair("Y", "yes")]);
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("printf '%s:%s' \"$X\" \"$Y\"")
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output()
            .expect("spawn /bin/sh");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "from_env:yes");
    }
}
