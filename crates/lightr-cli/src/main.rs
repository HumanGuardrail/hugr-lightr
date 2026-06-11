//! lightr — frozen CLI contract: build-spec v2 §7. Handlers are WP-5.
//! Exit law: 0 ok/clean · 1 dirty/runtime-error · 2 usage/not-found ·
//! `run` passes the child's exit code through.

mod exit;
mod handlers;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "lightr",
    version,
    about = "So light it isn't there. (native execution — reproducibility, not a sandbox)"
)]
struct Cli {
    /// Machine-readable output (stable keys)
    #[arg(long, global = true)]
    json: bool,
    /// Structured self-narration to stderr (memo keys, CoW rung, counts)
    #[arg(long, global = true)]
    explain: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

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
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Measure the indicator table on THIS machine
    Bench {
        #[arg(long)]
        vs_docker: bool,
        #[arg(long)]
        check: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    dispatch(cli);
}

fn dispatch(cli: Cli) -> ! {
    match cli.cmd {
        Cmd::Snapshot { dir, name } => handlers::snapshot::run(&dir, &name, cli.json, cli.explain),
        Cmd::Hydrate { dest, name, verify } => {
            handlers::hydrate::run(&dest, &name, verify, cli.json, cli.explain)
        }
        Cmd::Status { dir, name } => handlers::status::run(&dir, &name, cli.json, cli.explain),
        Cmd::Run {
            dir,
            input,
            env,
            command,
        } => handlers::run::run(&dir, &input, &env, &command, cli.json, cli.explain),
        Cmd::Bench { vs_docker, check } => handlers::bench::run(vs_docker, check, cli.json),
    }
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
            } => {
                assert_eq!(dir, ".");
                assert!(input.is_empty());
                assert!(env.is_empty());
                assert_eq!(command, &["echo", "hello"]);
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
    fn unknown_subcommand_fails() {
        assert!(try_parse(&["notaverb"]).is_err());
    }
}
