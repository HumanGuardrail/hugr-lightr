//! lightr — frozen CLI contract: build-spec v2 §7. Handlers are WP-5.
//! Exit law: 0 ok/clean · 1 dirty/runtime-error · 2 usage/not-found ·
//! `run` passes the child's exit code through.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lightr", version, about = "So light it isn't there. (native execution — reproducibility, not a sandbox)")]
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
    let _cli = Cli::parse();
    todo!("WP-5: dispatch per build-spec v2 §7")
}
