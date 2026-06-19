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

use cli::cmd::{Cli, Cmd, ComposeCmd, EngineCmd, OciCmd, SuperviseCmd};
pub use cli::cmd::PlanCmd;
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

fn main() {
    let cli = Cli::parse();

    // Determine verb name for event emitter
    let verb = match &cli.cmd {
        Cmd::Snapshot { .. } => "snapshot",
        Cmd::Hydrate { .. } => "hydrate",
        Cmd::Status { .. } => "status",
        Cmd::Run { .. } => "run",
        Cmd::Engine { subcmd } => match subcmd {
            EngineCmd::Ls => "engine-ls",
            EngineCmd::InstallPack { .. } => "engine-install-pack",
        },
        Cmd::Oci { subcmd } => match subcmd {
            OciCmd::Import { .. } => "oci-import",
            OciCmd::Pull { .. } => "oci-pull",
            OciCmd::Push { .. } => "oci-push",
        },
        Cmd::Bench { .. } => "bench",
        Cmd::Inspect { .. } => "inspect",
        Cmd::Ps { .. } => "ps",
        Cmd::Logs { .. } => "logs",
        Cmd::Stop { .. } => "stop",
        Cmd::Exec { .. } => "exec",
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
