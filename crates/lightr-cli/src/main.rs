//! lightr — frozen CLI contract: build-spec v2 §7. Handlers are WP-5.
//! Exit law: 0 ok/clean · 1 dirty/runtime-error · 2 usage/not-found ·
//! `run` passes the child's exit code through.

mod cli;
mod exit;
mod handlers;

/// Crate-shared serialization lock for in-process tests that mutate the
/// process-global `LIGHTR_HOME` env var. Take it for the whole
/// set_var → call → assert → remove_var critical section so parallel test
/// threads can't race on the shared environment. Poison-tolerant.
#[cfg(test)]
pub(crate) mod test_lock {
    pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

use clap::Parser;

pub use cli::cmd::PlanCmd;
use cli::cmd::{Cli, Cmd, ComposeCmd, EngineCmd, OciCmd, SuperviseCmd};
use cli::dispatch::dispatch;

// ──────────────────────────────────────────────────────────────────────────────
// Utility helpers
// ──────────────────────────────────────────────────────────────────────────────

pub fn lightr_home() -> std::path::PathBuf {
    if let Ok(h) = std::env::var("LIGHTR_HOME") {
        std::path::PathBuf::from(h)
    } else {
        let home = std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        home.join(".lightr")
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Event emitter
// ──────────────────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn emit_event(w: &mut impl std::io::Write, ev: &str, verb: &str, extra: &str) {
    let ts = now_ms();
    let _ = writeln!(w, r#"{{"ev":"{ev}","verb":"{verb}"{extra},"ts":{ts}}}"#);
}

// ──────────────────────────────────────────────────────────────────────────────
// Main + dispatch
// ──────────────────────────────────────────────────────────────────────────────

/// WP-NET3 — KEYSTONE: dispatch the switch-host re-exec marker BEFORE clap.
///
/// `lightr_run::vswitch::switch_host::attach` births the per-network L2 switch by
/// re-execing THIS binary with raw argv `[SWITCH_HOST_ARGV, <home>, <network_id>]`
/// (a `flock`-elected single birther). That argv is NOT a clap subcommand, so it
/// must be recognised here, before `Cli::parse()` — exactly as the c9-xproc-switch
/// example dispatches it — and routed into `run_switch_host`, the productionized
/// accept-loop + refcount self-stop. WITHOUT this, `attach` spawns a process that
/// clap rejects, the switch never binds its `ctl.sock`, and every `--network` vz
/// run times out: this is the keystone that makes the switch actually birth in the
/// real `lightr` binary.
///
/// Unix-only: `vswitch`/`switch_host` are `#[cfg(unix)]` (SCM_RIGHTS/socketpair/
/// flock are POSIX, and the whole vz path is unix). The marker is only ever
/// produced by `attach`, which is itself unix-only, so on windows there is nothing
/// to dispatch — fail-closed by construction (no marker is ever spawned).
/// WP-NET3: recognise the switch-host re-exec argv (pure, unit-testable). Returns
/// `Some((home, network_id))` iff `args` is `[exe, SWITCH_HOST_ARGV, home, id, ..]`
/// — the exact shape `switch_host::spawn_switch_host` produces. Kept separate from
/// the `process::exit` dispatch so the recognition can be tested without forking.
#[cfg(unix)]
fn switch_host_argv(args: &[String]) -> Option<(&str, &str)> {
    use lightr_run::vswitch::switch_host::SWITCH_HOST_ARGV;
    if args.len() >= 4 && args[1] == SWITCH_HOST_ARGV {
        Some((args[2].as_str(), args[3].as_str()))
    } else {
        None
    }
}

#[cfg(unix)]
fn maybe_dispatch_switch_host() {
    use lightr_run::vswitch::switch_host::run_switch_host;
    let args: Vec<String> = std::env::args().collect();
    if let Some((home, id)) = switch_host_argv(&args) {
        let _ = run_switch_host(std::path::Path::new(home), id);
        std::process::exit(0);
    }
}

#[cfg(not(unix))]
fn maybe_dispatch_switch_host() {}

fn main() {
    // WP-NET3 keystone: the switch-host re-exec marker is not a clap subcommand —
    // recognise + route it to `run_switch_host` before clap parses (mirrors the
    // `__supervise` re-exec dispatch + the c9-xproc-switch example). `attach`
    // only spawns this on unix; on windows it is a no-op.
    maybe_dispatch_switch_host();

    let cli = Cli::parse();

    // Determine verb name for event emitter
    let verb = match &cli.cmd {
        Cmd::Snapshot { .. } => "snapshot",
        Cmd::Hydrate { .. } => "hydrate",
        Cmd::Status { .. } => "status",
        Cmd::Run(_) => "run",
        Cmd::Engine { subcmd } => match subcmd {
            EngineCmd::Ls => "engine-ls",
            EngineCmd::InstallPack { .. } => "engine-install-pack",
        },
        Cmd::Oci { subcmd } => match subcmd {
            OciCmd::Import { .. } => "oci-import",
            OciCmd::Pull { .. } => "oci-pull",
            OciCmd::Push { .. } => "oci-push",
            OciCmd::Tag { .. } => "oci-tag",
            OciCmd::Save { .. } => "oci-save",
            OciCmd::Load { .. } => "oci-load",
            OciCmd::Images { .. } => "oci-images",
            OciCmd::Rmi { .. } => "oci-rmi",
            OciCmd::History { .. } => "oci-history",
        },
        Cmd::Bench { .. } => "bench",
        Cmd::Inspect { .. } => "inspect",
        Cmd::Ps { .. } => "ps",
        Cmd::Logs { .. } => "logs",
        Cmd::Stop { .. } => "stop",
        Cmd::Exec { .. } => "exec",
        Cmd::Rm { .. } => "rm",
        Cmd::Kill { .. } => "kill",
        Cmd::Start { .. } => "start",
        Cmd::Pause { .. } => "pause",
        Cmd::Unpause { .. } => "unpause",
        Cmd::Port { .. } => "port",
        Cmd::Restart { .. } => "restart",
        Cmd::Wait { .. } => "wait",
        Cmd::Rename { .. } => "rename",
        Cmd::Cp { .. } => "cp",
        Cmd::Stats { .. } => "stats",
        Cmd::Top { .. } => "top",
        Cmd::Network { .. } => "network",
        Cmd::Volume { .. } => "volume",
        Cmd::Images { .. } => "images",
        Cmd::Rmi { .. } => "rmi",
        Cmd::Tag { .. } => "tag",
        Cmd::History { .. } => "history",
        Cmd::Commit { .. } => "commit",
        Cmd::Version => "version",
        Cmd::Info => "info",
        Cmd::System { subcmd } => match subcmd {
            cli::cmd::SystemCmd::Df { .. } => "system-df",
            cli::cmd::SystemCmd::Prune { .. } => "system-prune",
        },
        Cmd::Gc { .. } => "gc",
        Cmd::Undo { .. } => "undo",
        Cmd::Diff { .. } => "diff",
        Cmd::Bisect { .. } => "bisect",
        Cmd::Plan { .. } => "plan",
        Cmd::Schema { .. } => "schema",
        Cmd::Mcp { .. } => "mcp",
        Cmd::Build { .. } => "build",
        Cmd::Compose { subcmd } => match subcmd {
            ComposeCmd::Up { .. } => "compose-up",
            ComposeCmd::Down { .. } => "compose-down",
        },
        Cmd::Docker { .. } => "docker",
        Cmd::Completions { .. } => "completions",
        Cmd::Man => "man",
        Cmd::Supervise { subcmd } => match subcmd {
            SuperviseCmd::Install { .. } => "supervise-install",
            SuperviseCmd::Uninstall { .. } => "supervise-uninstall",
            SuperviseCmd::List => "supervise-list",
        },
        Cmd::BenchCompare { .. } => "bench-compare",
        Cmd::SuperviseDetached { .. } => "__supervise",
        Cmd::ComposeSupervisor { .. } => "__compose-supervise",
    };

    if cli.events {
        emit_event(&mut std::io::stderr(), "start", verb, "");
    }

    let code = dispatch(cli.json, cli.explain, cli.events, verb, cli.cmd);

    // end event already emitted inside dispatch if needed
    std::process::exit(code);
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
