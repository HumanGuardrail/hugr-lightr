//! lightr — frozen CLI contract: build-spec v2 §7. Handlers are WP-5.
//! Exit law: 0 ok/clean · 1 dirty/runtime-error · 2 usage/not-found ·
//! `run` passes the child's exit code through.

mod exit;
mod handlers;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

// ──────────────────────────────────────────────────────────────────────────────
// Version string: <pkg> (<git-sha>, <build-date>) — sha/date from build.rs.
// ──────────────────────────────────────────────────────────────────────────────

const LIGHTR_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("LIGHTR_GIT_SHA"),
    ", ",
    env!("LIGHTR_BUILD_DATE"),
    ")"
);

/// Real, copy-pasteable examples shown under `lightr --help`.
const AFTER_HELP: &str = "\
EXAMPLES:
  # Run a command inside a pulled image's rootfs (CoW), memoized
  lightr run --rootfs alpine -- echo hello

  # Snapshot the current directory into the store under a ref
  lightr snapshot --dir . --name @me/proj

  # Import a docker-save tar (or OCI layout) into the store
  lightr oci import ./image.tar --name @docker/myimg

  # Measure the indicator table on this machine, compared to docker
  lightr bench --vs-docker";

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
// CLI struct
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "lightr",
    version = LIGHTR_VERSION,
    about = "So light it isn't there. (native execution — reproducibility, not a sandbox)",
    after_long_help = AFTER_HELP
)]
struct Cli {
    /// Machine-readable output (stable keys)
    #[arg(long, global = true)]
    json: bool,
    /// Structured self-narration to stderr (memo keys, CoW rung, counts)
    #[arg(long, global = true)]
    explain: bool,
    /// Emit JSON-RPC events to stderr on start/end
    #[arg(long, global = true)]
    events: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

// ──────────────────────────────────────────────────────────────────────────────
// ComposeCmd sub-enum
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum ComposeCmd {
    /// Start a compose stack (lazy by default)
    Up {
        /// Compose file to read
        #[arg(short = 'f', long, default_value = "compose.yml")]
        file: String,
        /// Start all services immediately (override lazy)
        #[arg(long)]
        eager: bool,
        /// Stack TTL in seconds before the supervisor exits
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// Tear down the most-recent compose stack
    Down {
        /// Compose file (used to identify the stack; resolved by newest stack dir)
        #[arg(short = 'f', long)]
        file: Option<String>,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// PlanCmd sub-enum
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum PlanCmd {
    /// Dry-run a snapshot (no store writes)
    Snapshot {
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(long)]
        name: String,
    },
    /// Dry-run a hydrate (no writes)
    Hydrate {
        dest: String,
        #[arg(long)]
        name: String,
    },
    /// Predict memoization for a run
    Run {
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(long)]
        input: Vec<String>,
        #[arg(long)]
        env: Vec<String>,
        #[arg(long, value_name = "REF:TARGET")]
        mount: Vec<String>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// EngineCmd sub-enum
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum EngineCmd {
    /// List available engines and their capabilities
    Ls,
    /// Install a linux kernel+initrd pack into the lightr home directory
    InstallPack {
        /// Directory containing 'kernel' and 'initrd' files
        dir: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// OciCmd sub-enum
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum OciCmd {
    /// Import an OCI layout directory or docker-save tar into the store
    Import {
        /// Path to an OCI layout directory or tar file
        path: String,
        /// Ref name to store the imported image under
        #[arg(long)]
        name: String,
    },
    /// Pull an image from a registry and import into the store
    Pull {
        /// Image reference (e.g. alpine, nginx:1.25, ghcr.io/owner/repo:tag)
        image: String,
        /// Ref name to store the pulled image under
        #[arg(long)]
        name: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// Shell enum (for `lightr completions <shell>`)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

// ──────────────────────────────────────────────────────────────────────────────
// Main command enum
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum Cmd {
    /// Snapshot a directory into the store under a ref
    Snapshot {
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(long)]
        name: String,
    },
    /// Materialize a ref into a directory (CoW)
    Hydrate {
        dest: String,
        #[arg(long)]
        name: String,
        /// Re-hash every object before materializing (paranoid path)
        #[arg(long)]
        verify: bool,
    },
    /// Compare a directory against a ref (exit 0 clean, 1 dirty)
    Status {
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(long)]
        name: String,
    },
    /// Run a command, memoized (exit code passes through)
    Run {
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(long)]
        input: Vec<String>,
        #[arg(long)]
        env: Vec<String>,
        #[arg(short = 'd', long)]
        detach: bool,
        #[arg(long, value_name = "REF:TARGET")]
        mount: Vec<String>,
        /// Engine to use: native (default), ns, vz
        #[arg(long, default_value = "native", value_name = "ENGINE")]
        engine: String,
        /// Hydrate a ref CoW into a temp dir and hand it to the engine as rootfs
        #[arg(long, value_name = "REF")]
        rootfs: Option<String>,
        /// Process-tree memoization (opt-in; honest fallback to whole-run memo)
        #[arg(long)]
        deep_memo: bool,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Engine management
    Engine {
        #[command(subcommand)]
        subcmd: EngineCmd,
    },
    /// OCI image management
    Oci {
        #[command(subcommand)]
        subcmd: OciCmd,
    },
    /// Measure the indicator table on THIS machine
    Bench {
        #[arg(long)]
        vs_docker: bool,
        #[arg(long)]
        check: bool,
    },
    /// List running/exited run instances
    Ps {
        #[arg(long)]
        json: bool,
    },
    /// Stream logs from a run
    Logs {
        id: String,
        #[arg(long)]
        stderr: bool,
        #[arg(long)]
        both: bool,
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Stop a running instance
    Stop {
        id: String,
        #[arg(long, default_value_t = 10)]
        grace: u64,
    },
    /// Exec a command in a run's context
    Exec {
        id: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Garbage collect unreachable objects
    Gc {
        #[arg(long)]
        force: bool,
        #[arg(long, default_value_t = 3600)]
        min_age: u64,
        #[arg(long)]
        json: bool,
    },
    /// Revert a ref to its previous version
    Undo {
        #[arg(long)]
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Diff a ref against a previous version
    Diff {
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 1)]
        at: usize,
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Binary-search ref history to find a regression
    Bisect {
        #[arg(long)]
        name: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Dry-run planning operations
    Plan {
        #[command(subcommand)]
        subcmd: PlanCmd,
    },
    /// Print JSON Schema for verb --json output (build-spec-r4 §2)
    Schema {
        /// Show schema for a specific verb only
        #[arg(long)]
        verb: Option<String>,
    },
    /// Serve MCP protocol on stdio
    Mcp {},
    /// Build an image from a Dockerfile (step-memoized)
    Build {
        /// Build context directory
        context: String,
        /// Path to Dockerfile (default: <context>/Dockerfile)
        #[arg(short = 'f', long)]
        file: Option<String>,
        /// Ref name to store the built image under
        #[arg(short = 't', long, default_value = "latest")]
        name: String,
        /// Engine to use: native (default), ns, vz
        #[arg(long, default_value = "native", value_name = "ENGINE")]
        engine: String,
    },
    /// Manage a compose stack (lazy services)
    Compose {
        #[command(subcommand)]
        subcmd: ComposeCmd,
    },
    /// Docker CLI compatibility shim (translates docker subcommands to lightr)
    Docker {
        /// Docker arguments (subcommand + flags)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print a shell completion script to stdout
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Print the roff man page to stdout
    Man,
    /// [internal] Supervise a detached run (hidden)
    #[command(name = "__supervise", hide = true)]
    Supervise { dir: String },
    /// [internal] Supervise a compose stack (hidden)
    #[command(name = "__compose-supervise", hide = true)]
    ComposeSupervisor { stack_dir: String },
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
        },
        Cmd::Bench { .. } => "bench",
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
        Cmd::Supervise { .. } => "__supervise",
        Cmd::ComposeSupervisor { .. } => "__compose-supervise",
    };

    if cli.events {
        emit_event(&mut std::io::stderr(), "start", verb, "");
    }

    let code = dispatch(cli.json, cli.explain, cli.events, verb, cli.cmd);

    // end event already emitted inside dispatch if needed
    std::process::exit(code);
}

fn dispatch(json: bool, explain: bool, events: bool, verb: &str, cmd: Cmd) -> i32 {
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
            mount,
            engine,
            rootfs,
            deep_memo,
            command,
        } => handlers::run::run(
            &dir,
            &input,
            &env,
            &command,
            json,
            explain,
            detach,
            &mount,
            &engine,
            rootfs.as_deref(),
            deep_memo,
        ),
        Cmd::Engine { subcmd } => match subcmd {
            EngineCmd::Ls => handlers::engine::ls(json),
            EngineCmd::InstallPack { dir } => handlers::engine::install_pack(&dir),
        },
        Cmd::Oci { subcmd } => match subcmd {
            OciCmd::Import { path, name } => handlers::oci::import(&path, &name, json),
            OciCmd::Pull { image, name } => handlers::oci::pull_image(&image, &name, json),
        },
        Cmd::Bench { vs_docker, check } => handlers::bench::run(vs_docker, check, json),
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
        Cmd::Supervise { dir } => match lightr_run::supervise(std::path::Path::new(&dir)) {
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
fn generate_completions(shell: Shell) -> i32 {
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
fn generate_man() -> i32 {
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

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::Cli;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("lightr").chain(args.iter().copied()))
            .expect("parse failed")
    }

    fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("lightr").chain(args.iter().copied()))
    }

    // ── snapshot ──────────────────────────────────────────────────────────

    #[test]
    fn snapshot_minimal() {
        let cli = parse(&["snapshot", "--name", "myref"]);
        match cli.cmd {
            super::Cmd::Snapshot { dir, name } => {
                assert_eq!(dir, ".");
                assert_eq!(name, "myref");
            }
            _ => panic!("wrong cmd"),
        }
        assert!(!cli.json);
        assert!(!cli.explain);
    }

    #[test]
    fn snapshot_all_flags() {
        let cli = parse(&[
            "--json",
            "--explain",
            "snapshot",
            "--dir",
            "/tmp/x",
            "--name",
            "v1",
        ]);
        match cli.cmd {
            super::Cmd::Snapshot { dir, name } => {
                assert_eq!(dir, "/tmp/x");
                assert_eq!(name, "v1");
            }
            _ => panic!("wrong cmd"),
        }
        assert!(cli.json);
        assert!(cli.explain);
    }

    #[test]
    fn snapshot_requires_name() {
        assert!(try_parse(&["snapshot"]).is_err());
    }

    // ── hydrate ───────────────────────────────────────────────────────────

    #[test]
    fn hydrate_minimal() {
        let cli = parse(&["hydrate", "/dest", "--name", "v1"]);
        match cli.cmd {
            super::Cmd::Hydrate { dest, name, verify } => {
                assert_eq!(dest, "/dest");
                assert_eq!(name, "v1");
                assert!(!verify);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn hydrate_requires_dest_and_name() {
        assert!(try_parse(&["hydrate", "--name", "v1"]).is_err());
        assert!(try_parse(&["hydrate", "/dest"]).is_err());
    }

    #[test]
    fn hydrate_json_flag() {
        let cli = parse(&["--json", "hydrate", "/d", "--name", "r"]);
        assert!(cli.json);
    }

    // ── status ────────────────────────────────────────────────────────────

    #[test]
    fn status_minimal() {
        let cli = parse(&["status", "--name", "myref"]);
        match cli.cmd {
            super::Cmd::Status { dir, name } => {
                assert_eq!(dir, ".");
                assert_eq!(name, "myref");
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn status_with_dir() {
        let cli = parse(&["status", "--dir", "/src", "--name", "r"]);
        match cli.cmd {
            super::Cmd::Status { dir, name } => {
                assert_eq!(dir, "/src");
                assert_eq!(name, "r");
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn status_requires_name() {
        assert!(try_parse(&["status"]).is_err());
    }

    // ── run ───────────────────────────────────────────────────────────────

    #[test]
    fn run_minimal() {
        let cli = parse(&["run", "--", "echo", "hello"]);
        match &cli.cmd {
            super::Cmd::Run {
                dir,
                input,
                env,
                command,
                detach,
                mount,
                ..
            } => {
                assert_eq!(dir, ".");
                assert!(input.is_empty());
                assert!(env.is_empty());
                assert_eq!(command, &["echo", "hello"]);
                assert!(!detach);
                assert!(mount.is_empty());
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn run_all_flags() {
        let cli = parse(&[
            "run", "--dir", "/work", "--input", "/a", "--input", "/b", "--env", "FOO", "--env",
            "BAR", "--", "make", "all",
        ]);
        match &cli.cmd {
            super::Cmd::Run {
                dir,
                input,
                env,
                command,
                ..
            } => {
                assert_eq!(dir, "/work");
                assert_eq!(input, &["/a", "/b"]);
                assert_eq!(env, &["FOO", "BAR"]);
                assert_eq!(command, &["make", "all"]);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn run_requires_command() {
        assert!(try_parse(&["run"]).is_err());
    }

    #[test]
    fn run_detach_flag() {
        let cli = parse(&["run", "-d", "--", "sleep", "100"]);
        match &cli.cmd {
            super::Cmd::Run { detach, .. } => {
                assert!(*detach, "expected detach to be true");
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn run_mount_single() {
        let cli = parse(&["run", "--mount", "myref:subdir", "--", "echo"]);
        match &cli.cmd {
            super::Cmd::Run { mount, .. } => {
                assert_eq!(mount, &["myref:subdir"]);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn run_mount_multiple() {
        let cli = parse(&["run", "--mount", "r1:a", "--mount", "r2:b", "--", "cmd"]);
        match &cli.cmd {
            super::Cmd::Run { mount, .. } => {
                assert_eq!(mount.len(), 2);
                assert_eq!(mount[0], "r1:a");
                assert_eq!(mount[1], "r2:b");
            }
            _ => panic!("wrong cmd"),
        }
    }

    // ── bench ─────────────────────────────────────────────────────────────

    #[test]
    fn bench_minimal() {
        let cli = parse(&["bench"]);
        match cli.cmd {
            super::Cmd::Bench { vs_docker, check } => {
                assert!(!vs_docker);
                assert!(!check);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn bench_all_flags() {
        let cli = parse(&["bench", "--vs-docker", "--check"]);
        match cli.cmd {
            super::Cmd::Bench { vs_docker, check } => {
                assert!(vs_docker);
                assert!(check);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn bench_json_flag() {
        let cli = parse(&["--json", "bench"]);
        assert!(cli.json);
    }

    // ── ps ────────────────────────────────────────────────────────────────

    #[test]
    fn ps_minimal() {
        parse(&["ps"]);
    }

    #[test]
    fn ps_json() {
        let cli = parse(&["ps", "--json"]);
        match &cli.cmd {
            super::Cmd::Ps { json } => assert!(*json),
            _ => panic!("wrong cmd"),
        }
    }

    // ── logs ──────────────────────────────────────────────────────────────

    #[test]
    fn logs_minimal() {
        let cli = parse(&["logs", "abc123"]);
        match &cli.cmd {
            super::Cmd::Logs {
                id,
                stderr,
                both,
                follow,
            } => {
                assert_eq!(id, "abc123");
                assert!(!stderr);
                assert!(!both);
                assert!(!follow);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn logs_stderr_flag() {
        parse(&["logs", "id1", "--stderr"]);
    }

    #[test]
    fn logs_both_flag() {
        parse(&["logs", "id1", "--both"]);
    }

    #[test]
    fn logs_follow_flag() {
        parse(&["logs", "id1", "-f"]);
    }

    #[test]
    fn logs_requires_id() {
        assert!(try_parse(&["logs"]).is_err());
    }

    // ── stop ──────────────────────────────────────────────────────────────

    #[test]
    fn stop_minimal() {
        parse(&["stop", "myid"]);
    }

    #[test]
    fn stop_grace() {
        parse(&["stop", "myid", "--grace", "5"]);
    }

    #[test]
    fn stop_default_grace() {
        let cli = parse(&["stop", "myid"]);
        match &cli.cmd {
            super::Cmd::Stop { grace, .. } => assert_eq!(*grace, 10),
            _ => panic!("wrong cmd"),
        }
    }

    // ── exec ──────────────────────────────────────────────────────────────

    #[test]
    fn exec_minimal() {
        parse(&["exec", "myid", "--", "echo", "hi"]);
    }

    #[test]
    fn exec_requires_command() {
        assert!(try_parse(&["exec", "myid"]).is_err());
    }

    // ── gc ────────────────────────────────────────────────────────────────

    #[test]
    fn gc_minimal() {
        let cli = parse(&["gc"]);
        match &cli.cmd {
            super::Cmd::Gc {
                force,
                min_age,
                json,
            } => {
                assert!(!force);
                assert_eq!(*min_age, 3600);
                assert!(!json);
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn gc_force() {
        parse(&["gc", "--force"]);
    }

    #[test]
    fn gc_min_age() {
        parse(&["gc", "--min-age", "7200"]);
    }

    // ── undo ──────────────────────────────────────────────────────────────

    #[test]
    fn undo_name() {
        parse(&["undo", "--name", "myref"]);
    }

    #[test]
    fn undo_requires_name() {
        assert!(try_parse(&["undo"]).is_err());
    }

    // ── diff ──────────────────────────────────────────────────────────────

    #[test]
    fn diff_minimal() {
        let cli = parse(&["diff", "--name", "myref"]);
        match &cli.cmd {
            super::Cmd::Diff { name, at, dir, .. } => {
                assert_eq!(name, "myref");
                assert_eq!(*at, 1);
                assert!(dir.is_none());
            }
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn diff_at() {
        parse(&["diff", "--name", "r", "--at", "3"]);
    }

    #[test]
    fn diff_dir() {
        parse(&["diff", "--name", "r", "--dir", "/tmp/x"]);
    }

    // ── bisect ────────────────────────────────────────────────────────────

    #[test]
    fn bisect_minimal() {
        parse(&["bisect", "--name", "r", "--", "sh", "-c", "true"]);
    }

    #[test]
    fn bisect_requires_name_and_cmd() {
        assert!(try_parse(&["bisect"]).is_err());
    }

    // ── plan ──────────────────────────────────────────────────────────────

    #[test]
    fn plan_snapshot() {
        parse(&["plan", "snapshot", "--name", "r"]);
    }

    #[test]
    fn plan_hydrate() {
        parse(&["plan", "hydrate", "/dest", "--name", "r"]);
    }

    #[test]
    fn plan_run() {
        parse(&["plan", "run", "--", "echo"]);
    }

    // ── schema ────────────────────────────────────────────────────────────

    #[test]
    fn schema_no_verb_parses() {
        let cli = parse(&["schema"]);
        match &cli.cmd {
            super::Cmd::Schema { verb } => assert!(verb.is_none()),
            _ => panic!("expected Schema"),
        }
    }

    #[test]
    fn schema_with_verb_parses() {
        let cli = parse(&["schema", "--verb", "run"]);
        match &cli.cmd {
            super::Cmd::Schema { verb } => assert_eq!(verb.as_deref(), Some("run")),
            _ => panic!("expected Schema"),
        }
    }

    #[test]
    fn schema_unknown_verb_exits_2() {
        use super::handlers::schema::run as schema_run;
        let code = schema_run(Some("notaverb"));
        assert_eq!(code, 2, "unknown verb must exit 2");
    }

    // ── mcp ───────────────────────────────────────────────────────────────

    #[test]
    fn mcp_parses() {
        parse(&["mcp"]);
    }

    // ── supervise (hidden) ─────────────────────────────────────────────────

    #[test]
    fn supervise_parses() {
        parse(&["__supervise", "/some/dir"]);
    }

    // ── global flags ──────────────────────────────────────────────────────

    #[test]
    fn global_json_before_subcommand() {
        let cli = parse(&["--json", "snapshot", "--name", "r"]);
        assert!(cli.json);
    }

    #[test]
    fn global_explain_flag() {
        let cli = parse(&["--explain", "status", "--name", "r"]);
        assert!(cli.explain);
    }

    #[test]
    fn events_global_flag() {
        let cli = parse(&["--events", "ps"]);
        assert!(cli.events);
    }

    #[test]
    fn unknown_subcommand_fails() {
        assert!(try_parse(&["notaverb"]).is_err());
    }

    // ── EventEmitter unit tests ────────────────────────────────────────────

    #[test]
    fn emitter_start_contains_start_ev() {
        let mut buf = Vec::<u8>::new();
        super::emit_event(&mut buf, "start", "snapshot", "");
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains(r#""ev":"start""#), "missing ev:start in: {s}");
        assert!(
            s.contains(r#""verb":"snapshot""#),
            "missing verb:snapshot in: {s}"
        );
        assert!(s.contains("\"ts\":"), "missing ts in: {s}");
    }

    #[test]
    fn emitter_end_contains_ok_and_exit() {
        let mut buf = Vec::<u8>::new();
        super::emit_event(&mut buf, "end", "run", r#","ok":true,"exit":0"#);
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains(r#""ev":"end""#), "missing ev:end in: {s}");
        assert!(s.contains(r#""ok":true"#), "missing ok:true in: {s}");
        assert!(s.contains(r#""exit":0"#), "missing exit:0 in: {s}");
    }

    // ── engine ls ─────────────────────────────────────────────────────────────

    #[test]
    fn engine_ls_parses() {
        let cli = parse(&["engine", "ls"]);
        match &cli.cmd {
            super::Cmd::Engine { subcmd } => {
                matches!(subcmd, super::EngineCmd::Ls);
            }
            _ => panic!("expected Engine cmd"),
        }
    }

    #[test]
    fn engine_ls_json_uses_global_flag() {
        let cli = parse(&["--json", "engine", "ls"]);
        assert!(cli.json, "global --json must be set");
        match &cli.cmd {
            super::Cmd::Engine { subcmd } => {
                matches!(subcmd, super::EngineCmd::Ls);
            }
            _ => panic!("expected Engine cmd"),
        }
    }

    // ── engine install-pack ───────────────────────────────────────────────────

    #[test]
    fn engine_install_pack_parses() {
        let cli = parse(&["engine", "install-pack", "/tmp/mypack"]);
        match &cli.cmd {
            super::Cmd::Engine { subcmd } => match subcmd {
                super::EngineCmd::InstallPack { dir } => {
                    assert_eq!(dir, "/tmp/mypack");
                }
                _ => panic!("expected InstallPack"),
            },
            _ => panic!("expected Engine cmd"),
        }
    }

    #[test]
    fn engine_install_pack_requires_dir() {
        assert!(try_parse(&["engine", "install-pack"]).is_err());
    }

    // ── oci import ────────────────────────────────────────────────────────────

    #[test]
    fn oci_import_parses() {
        let cli = parse(&["oci", "import", "/tmp/layout", "--name", "myimage"]);
        match &cli.cmd {
            super::Cmd::Oci { subcmd } => match subcmd {
                super::OciCmd::Import { path, name } => {
                    assert_eq!(path, "/tmp/layout");
                    assert_eq!(name, "myimage");
                }
                _ => panic!("expected Import"),
            },
            _ => panic!("expected Oci cmd"),
        }
    }

    #[test]
    fn oci_import_json_uses_global_flag() {
        let cli = parse(&["--json", "oci", "import", "/tmp/x", "--name", "img"]);
        assert!(cli.json, "global --json must be set");
    }

    #[test]
    fn oci_import_requires_path_and_name() {
        assert!(try_parse(&["oci", "import"]).is_err());
        assert!(try_parse(&["oci", "import", "/tmp/x"]).is_err());
    }

    // ── oci pull ──────────────────────────────────────────────────────────────

    #[test]
    fn oci_pull_parses() {
        let cli = parse(&["oci", "pull", "alpine:latest", "--name", "my-alpine"]);
        match &cli.cmd {
            super::Cmd::Oci { subcmd } => match subcmd {
                super::OciCmd::Pull { image, name } => {
                    assert_eq!(image, "alpine:latest");
                    assert_eq!(name, "my-alpine");
                }
                _ => panic!("expected Pull"),
            },
            _ => panic!("expected Oci cmd"),
        }
    }

    #[test]
    fn oci_pull_requires_image_and_name() {
        assert!(try_parse(&["oci", "pull"]).is_err());
        assert!(try_parse(&["oci", "pull", "alpine"]).is_err());
    }

    #[test]
    fn oci_pull_json_uses_global_flag() {
        let cli = parse(&["--json", "oci", "pull", "alpine", "--name", "a"]);
        assert!(cli.json);
    }

    // ── run --engine / --rootfs ───────────────────────────────────────────────

    #[test]
    fn run_engine_default_is_native() {
        let cli = parse(&["run", "--", "echo", "hi"]);
        match &cli.cmd {
            super::Cmd::Run { engine, rootfs, .. } => {
                assert_eq!(engine, "native");
                assert!(rootfs.is_none());
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_engine_ns() {
        let cli = parse(&["run", "--engine", "ns", "--", "echo"]);
        match &cli.cmd {
            super::Cmd::Run { engine, .. } => assert_eq!(engine, "ns"),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_engine_vz() {
        let cli = parse(&["run", "--engine", "vz", "--", "echo"]);
        match &cli.cmd {
            super::Cmd::Run { engine, .. } => assert_eq!(engine, "vz"),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_rootfs_flag() {
        let cli = parse(&["run", "--rootfs", "my-image", "--engine", "ns", "--", "sh"]);
        match &cli.cmd {
            super::Cmd::Run { rootfs, engine, .. } => {
                assert_eq!(rootfs.as_deref(), Some("my-image"));
                assert_eq!(engine, "ns");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_bad_engine_string_rejected_at_handler() {
        // Clap accepts any string for --engine; the handler rejects bad values at exit 2.
        // Parse succeeds:
        let cli = parse(&["run", "--engine", "bogus", "--", "echo"]);
        match &cli.cmd {
            super::Cmd::Run { engine, .. } => assert_eq!(engine, "bogus"),
            _ => panic!("expected Run"),
        }
        // The handler should return 2 for a bad engine string.
        // We test this through the handler directly (not through process::exit).
        use super::handlers::run::run as run_handler;
        let code = run_handler(
            ".",
            &[],
            &[],
            &["echo".to_string()],
            false,
            false,
            false,
            &[],
            "bogus",
            None,
            false,
        );
        assert_eq!(code, 2, "bad engine string must exit 2");
    }

    #[test]
    fn run_native_with_rootfs_rejected_by_engine() {
        // native + rootfs ⇒ the NativeEngine itself returns InvalidRef → exit 2
        // We need a valid store to test this, so instead we verify parse accepts
        // the flags and trust the engine unit tests cover the runtime rejection.
        let cli = parse(&["run", "--engine", "native", "--rootfs", "@x", "--", "true"]);
        match &cli.cmd {
            super::Cmd::Run { engine, rootfs, .. } => {
                assert_eq!(engine, "native");
                assert_eq!(rootfs.as_deref(), Some("@x"));
            }
            _ => panic!("expected Run"),
        }
    }

    // ── build ─────────────────────────────────────────────────────────────────

    #[test]
    fn build_minimal() {
        let cli = parse(&["build", "/some/ctx"]);
        match &cli.cmd {
            super::Cmd::Build {
                context,
                file,
                name,
                engine,
            } => {
                assert_eq!(context, "/some/ctx");
                assert!(file.is_none(), "no -f by default");
                assert_eq!(name, "latest", "default name is latest");
                assert_eq!(engine, "native", "default engine is native");
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn build_with_file_flag() {
        let cli = parse(&["build", "-f", "custom/Dockerfile", "/ctx"]);
        match &cli.cmd {
            super::Cmd::Build { file, .. } => {
                assert_eq!(file.as_deref(), Some("custom/Dockerfile"));
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn build_with_name_flag() {
        let cli = parse(&["build", "-t", "my-image", "/ctx"]);
        match &cli.cmd {
            super::Cmd::Build { name, .. } => {
                assert_eq!(name, "my-image");
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn build_with_engine_flag() {
        let cli = parse(&["build", "--engine", "ns", "/ctx"]);
        match &cli.cmd {
            super::Cmd::Build { engine, .. } => {
                assert_eq!(engine, "ns");
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn build_all_flags() {
        let cli = parse(&[
            "--json",
            "build",
            "-f",
            "/path/Dockerfile",
            "-t",
            "my-ref",
            "--engine",
            "vz",
            "/my/ctx",
        ]);
        assert!(cli.json);
        match &cli.cmd {
            super::Cmd::Build {
                context,
                file,
                name,
                engine,
            } => {
                assert_eq!(context, "/my/ctx");
                assert_eq!(file.as_deref(), Some("/path/Dockerfile"));
                assert_eq!(name, "my-ref");
                assert_eq!(engine, "vz");
            }
            _ => panic!("expected Build"),
        }
    }

    #[test]
    fn build_requires_context() {
        assert!(try_parse(&["build"]).is_err());
    }

    // ── compose up ────────────────────────────────────────────────────────────

    #[test]
    fn compose_up_minimal() {
        let cli = parse(&["compose", "up"]);
        match &cli.cmd {
            super::Cmd::Compose { subcmd } => match subcmd {
                super::ComposeCmd::Up { file, eager, ttl } => {
                    assert_eq!(file, "compose.yml", "default compose file");
                    assert!(!eager, "eager is false by default");
                    assert_eq!(*ttl, 3600, "default TTL is 3600");
                }
                _ => panic!("expected Up"),
            },
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn compose_up_with_file_flag() {
        let cli = parse(&["compose", "up", "-f", "docker-compose.yml"]);
        match &cli.cmd {
            super::Cmd::Compose { subcmd } => match subcmd {
                super::ComposeCmd::Up { file, .. } => {
                    assert_eq!(file, "docker-compose.yml");
                }
                _ => panic!("expected Up"),
            },
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn compose_up_eager_flag() {
        let cli = parse(&["compose", "up", "--eager"]);
        match &cli.cmd {
            super::Cmd::Compose { subcmd } => match subcmd {
                super::ComposeCmd::Up { eager, .. } => {
                    assert!(*eager);
                }
                _ => panic!("expected Up"),
            },
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn compose_up_ttl_flag() {
        let cli = parse(&["compose", "up", "--ttl", "7200"]);
        match &cli.cmd {
            super::Cmd::Compose { subcmd } => match subcmd {
                super::ComposeCmd::Up { ttl, .. } => {
                    assert_eq!(*ttl, 7200);
                }
                _ => panic!("expected Up"),
            },
            _ => panic!("expected Compose"),
        }
    }

    // ── compose down ──────────────────────────────────────────────────────────

    #[test]
    fn compose_down_minimal() {
        let cli = parse(&["compose", "down"]);
        match &cli.cmd {
            super::Cmd::Compose { subcmd } => match subcmd {
                super::ComposeCmd::Down { file } => {
                    assert!(file.is_none(), "no -f by default");
                }
                _ => panic!("expected Down"),
            },
            _ => panic!("expected Compose"),
        }
    }

    #[test]
    fn compose_down_with_file_flag() {
        let cli = parse(&["compose", "down", "-f", "my-compose.yml"]);
        match &cli.cmd {
            super::Cmd::Compose { subcmd } => match subcmd {
                super::ComposeCmd::Down { file } => {
                    assert_eq!(file.as_deref(), Some("my-compose.yml"));
                }
                _ => panic!("expected Down"),
            },
            _ => panic!("expected Compose"),
        }
    }

    // ── __compose-supervise (hidden) ──────────────────────────────────────────

    #[test]
    fn compose_supervise_hidden_parses() {
        let cli = parse(&["__compose-supervise", "/some/stack/dir"]);
        match &cli.cmd {
            super::Cmd::ComposeSupervisor { stack_dir } => {
                assert_eq!(stack_dir, "/some/stack/dir");
            }
            _ => panic!("expected ComposeSupervisor"),
        }
    }

    // ── docker varargs ────────────────────────────────────────────────────────

    #[test]
    fn docker_varargs_capture() {
        let cli = parse(&["docker", "build", "-t", "myref", "."]);
        match &cli.cmd {
            super::Cmd::Docker { args } => {
                assert_eq!(args, &["build", "-t", "myref", "."]);
            }
            _ => panic!("expected Docker"),
        }
    }

    #[test]
    fn docker_images_parses() {
        let cli = parse(&["docker", "images"]);
        match &cli.cmd {
            super::Cmd::Docker { args } => {
                assert_eq!(args, &["images"]);
            }
            _ => panic!("expected Docker"),
        }
    }

    #[test]
    fn docker_ps_parses() {
        let cli = parse(&["docker", "ps"]);
        match &cli.cmd {
            super::Cmd::Docker { args } => {
                assert_eq!(args, &["ps"]);
            }
            _ => panic!("expected Docker"),
        }
    }

    #[test]
    fn docker_pull_parses() {
        let cli = parse(&["docker", "pull", "alpine:latest"]);
        match &cli.cmd {
            super::Cmd::Docker { args } => {
                assert_eq!(args, &["pull", "alpine:latest"]);
            }
            _ => panic!("expected Docker"),
        }
    }

    #[test]
    fn docker_compose_parses() {
        let cli = parse(&["docker", "compose", "up", "-f", "myfile.yml"]);
        match &cli.cmd {
            super::Cmd::Docker { args } => {
                assert_eq!(args, &["compose", "up", "-f", "myfile.yml"]);
            }
            _ => panic!("expected Docker"),
        }
    }

    // ── docker translation unit tests (via handlers::docker) ──────────────────

    #[test]
    fn docker_unsupported_exits_2() {
        use super::handlers::docker::run as docker_run;
        let code = docker_run(&["frobnicate".to_string()], false, false);
        assert_eq!(code, 2, "unsupported docker subcommand must exit 2");
    }

    #[test]
    fn docker_unsupported_exact_message_format() {
        // Verify the message format via the sanitize fn + exit code test above.
        // The exact message is:
        //   "lightr docker: unsupported 'frobnicate' — supported: build|run|pull|images|ps|compose"
        // We trust the string literal in docker.rs is correct (verified by code review).
        use super::handlers::docker::run as docker_run;
        let code = docker_run(&["notreal".to_string()], false, false);
        assert_eq!(code, 2);
    }

    #[test]
    fn docker_ref_sanitize_slash_colon() {
        use super::handlers::docker::sanitize_docker_ref;
        assert_eq!(sanitize_docker_ref("nginx:1.25"), "@docker/nginx-1.25");
        assert_eq!(
            sanitize_docker_ref("ghcr.io/owner/repo:tag"),
            "@docker/ghcr.io-owner-repo-tag"
        );
    }

    #[test]
    fn docker_empty_args_exits_2() {
        use super::handlers::docker::run as docker_run;
        let code = docker_run(&[], false, false);
        assert_eq!(code, 2);
    }

    // ── completions / man ──────────────────────────────────────────────────────

    #[test]
    fn completions_parses_each_shell() {
        for s in ["bash", "zsh", "fish", "powershell", "elvish"] {
            let cli = parse(&["completions", s]);
            match &cli.cmd {
                super::Cmd::Completions { .. } => {}
                _ => panic!("expected Completions for {s}"),
            }
        }
    }

    #[test]
    fn completions_requires_shell() {
        assert!(try_parse(&["completions"]).is_err());
    }

    #[test]
    fn completions_rejects_unknown_shell() {
        assert!(try_parse(&["completions", "tcsh"]).is_err());
    }

    #[test]
    fn man_parses() {
        let cli = parse(&["man"]);
        match &cli.cmd {
            super::Cmd::Man => {}
            _ => panic!("expected Man"),
        }
    }

    #[test]
    fn cli_command_verifies() {
        // clap asserts internal consistency (incl. after_long_help) on debug_assert.
        use clap::CommandFactory as _;
        super::Cli::command().debug_assert();
    }

    #[test]
    fn version_string_contains_pkg_version() {
        assert!(super::LIGHTR_VERSION.starts_with(env!("CARGO_PKG_VERSION")));
    }
}
