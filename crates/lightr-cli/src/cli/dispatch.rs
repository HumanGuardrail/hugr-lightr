//! Dispatch: routes a parsed `Cmd` to the appropriate handler.
//! Also owns the completions + man-page generators (they need `Cli::command()`).
//! PURE MOVE from main.rs.

use crate::cli::cmd::{Cli, Cmd, ComposeCmd, EngineCmd, OciCmd, Shell, SuperviseCmd};
use crate::emit_event;
use crate::handlers;

// ──────────────────────────────────────────────────────────────────────────────
// Dispatch
// ──────────────────────────────────────────────────────────────────────────────

pub(crate) fn dispatch(json: bool, explain: bool, events: bool, verb: &str, cmd: Cmd) -> i32 {
    let code = match cmd {
        Cmd::Snapshot { dir, name } => handlers::snapshot::run(&dir, &name, json, explain),
        Cmd::Hydrate { dest, name, verify } => {
            handlers::hydrate::run(&dest, &name, verify, json, explain)
        }
        Cmd::Status { dir, name } => handlers::status::run(&dir, &name, json, explain),
        Cmd::Run {
            dir,
            input,
            env,
            detach,
            publish,
            mount,
            engine,
            rootfs,
            deep_memo,
            memory,
            cpus,
            secret,
            config,
            health_cmd,
            health_interval,
            health_retries,
            command,
        } => handlers::run::run(
            &dir,
            &input,
            &env,
            &command,
            json,
            explain,
            detach,
            &publish,
            &mount,
            &engine,
            rootfs.as_deref(),
            deep_memo,
            memory.as_deref(),
            cpus.as_deref(),
            &secret,
            &config,
            health_cmd.as_deref(),
            health_interval,
            health_retries,
        ),
        Cmd::Engine { subcmd } => match subcmd {
            EngineCmd::Ls => handlers::engine::ls(json),
            EngineCmd::InstallPack { dir } => handlers::engine::install_pack(&dir),
        },
        Cmd::Oci { subcmd } => match subcmd {
            OciCmd::Import { path, name } => handlers::oci::import(&path, &name, json),
            OciCmd::Pull { image, name } => handlers::oci::pull_image(&image, &name, json),
            OciCmd::Push { store_ref, target } => {
                handlers::oci::push_image(&store_ref, &target, json)
            }
        },
        Cmd::Bench { vs_docker, check } => handlers::bench::run(vs_docker, check, json),
        Cmd::Inspect { id } => handlers::inspect::run(&id, json),
        Cmd::Ps { json: ps_json } => handlers::ps::run(ps_json),
        Cmd::Logs {
            id,
            stderr,
            both,
            follow,
        } => handlers::logs::run(&id, stderr, both, follow),
        Cmd::Stop { id, grace } => handlers::stop::run(&id, grace),
        Cmd::Exec { id, command } => handlers::exec::run(&id, &command),
        Cmd::Gc {
            force,
            min_age,
            json: gc_json,
        } => handlers::gc::run(force, min_age, gc_json),
        Cmd::Undo {
            name,
            json: undo_json,
        } => handlers::undo::run(&name, undo_json),
        Cmd::Diff {
            name,
            at,
            dir,
            json: diff_json,
        } => handlers::diff::run(&name, at, dir.as_deref(), diff_json),
        Cmd::Bisect {
            name,
            command,
            json: bisect_json,
        } => handlers::bisect::run(&name, &command, bisect_json),
        Cmd::Plan { subcmd } => handlers::plan::run(subcmd),
        Cmd::Schema { verb } => handlers::schema::run(verb.as_deref()),
        Cmd::Mcp {} => handlers::mcp::run(),
        Cmd::Build {
            context,
            file,
            name,
            engine,
        } => handlers::build::run(&context, file.as_deref(), &name, &engine, json, explain),
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Up { file, eager, ttl } => handlers::compose::up(&file, eager, ttl, json),
            ComposeCmd::Down { file } => handlers::compose::down(file.as_deref()),
        },
        Cmd::Docker { args } => handlers::docker::run(&args, json, explain),
        Cmd::Completions { shell } => generate_completions(shell),
        Cmd::Man => generate_man(),
        Cmd::Supervise { subcmd } => match subcmd {
            SuperviseCmd::Install {
                name,
                restart,
                dir,
                command,
            } => handlers::supervise::install(&name, &restart, &dir, &command),
            SuperviseCmd::Uninstall { name } => handlers::supervise::uninstall(&name),
            SuperviseCmd::List => handlers::supervise::list(),
        },
        Cmd::BenchCompare { vs, workload, json } => {
            handlers::bench_compare::run(&vs, &workload, json)
        }
        Cmd::SuperviseDetached { dir } => match lightr_run::supervise(std::path::Path::new(&dir)) {
            Ok(exit_code) => exit_code,
            Err(e) => {
                eprintln!("lightr: supervise error: {e}");
                2
            }
        },
        Cmd::ComposeSupervisor { stack_dir } => {
            match lightr_build::compose_supervise(std::path::Path::new(&stack_dir)) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("lightr: compose-supervise error: {e}");
                    1
                }
            }
        }
    };

    if events {
        let ok = code == 0;
        let extra = format!(r#","ok":{ok},"exit":{code}"#);
        emit_event(&mut std::io::stderr(), "end", verb, &extra);
    }

    code
}

// ──────────────────────────────────────────────────────────────────────────────
// completions / man generators
// ──────────────────────────────────────────────────────────────────────────────

/// Write the shell completion script for `shell` to stdout.
pub(crate) fn generate_completions(shell: Shell) -> i32 {
    use clap::CommandFactory;
    use clap_complete::Shell as CcShell;
    let cc: CcShell = match shell {
        Shell::Bash => CcShell::Bash,
        Shell::Zsh => CcShell::Zsh,
        Shell::Fish => CcShell::Fish,
        Shell::Powershell => CcShell::PowerShell,
        Shell::Elvish => CcShell::Elvish,
    };
    let mut cmd = Cli::command();
    let bin = cmd.get_name().to_string();
    clap_complete::generate(cc, &mut cmd, bin, &mut std::io::stdout());
    0
}

/// Render the roff man page for the top-level command to stdout.
pub(crate) fn generate_man() -> i32 {
    use clap::CommandFactory;
    let man = clap_mangen::Man::new(Cli::command());
    let mut out = Vec::new();
    if let Err(e) = man.render(&mut out) {
        eprintln!("lightr: man render error: {e}");
        return 1;
    }
    use std::io::Write as _;
    if std::io::stdout().write_all(&out).is_err() {
        return 1;
    }
    0
}
