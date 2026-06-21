//! CLI enum definitions — clap `Parser`/`Subcommand`/`ValueEnum` derives.
//! PURE MOVE from cmd.rs: every attribute and doc-comment preserved verbatim.

use clap::{Parser, Subcommand};

use crate::cli::version::{AFTER_HELP, LIGHTR_VERSION};

mod run_args;
mod subcommands;
pub(crate) use run_args::RunArgs;
pub use subcommands::*;

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
pub(crate) struct Cli {
    /// Machine-readable output (stable keys)
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Structured self-narration to stderr (memo keys, CoW rung, counts)
    #[arg(long, global = true)]
    pub(crate) explain: bool,
    /// Emit JSON-RPC events to stderr on start/end
    #[arg(long, global = true)]
    pub(crate) events: bool,
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

// ──────────────────────────────────────────────────────────────────────────────
// Main command enum
// ──────────────────────────────────────────────────────────────────────────────

// `Cmd` is a clap dispatch enum: constructed exactly once per process (at
// argv parse) and immediately matched. The `run` flag surface lives in a
// flattened `#[derive(Args)] RunArgs` sub-struct (`run_args.rs`) so this enum
// keeps lasting headroom under the 400-line cap and future run-flag WPs edit
// the sub-struct, not this variant.
//
// The flattened `Run(RunArgs)` is still the largest variant (~40 fields), so it
// trips clippy's `large_enum_variant` default just as the inline variant did.
// Boxing a clap field to satisfy a memory-layout lint on a once-per-process,
// immediately-matched value is non-idiomatic and would distort the parse
// surface, so we allow the lint here rather than indirect the field.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub(crate) enum Cmd {
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
    Run(RunArgs),
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
    /// Inspect a run instance (docker-inspect parity)
    Inspect {
        /// Run id to inspect
        id: String,
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
    // ── Docker-parity container-lifecycle verbs (CLI-surface freeze) ───────────
    /// Remove one or more containers (docker rm)
    Rm {
        targets: Vec<String>,
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Send a signal to one or more running containers (docker kill)
    Kill {
        targets: Vec<String>,
        #[arg(short = 's', long)]
        signal: Option<String>,
    },
    /// Start one or more stopped containers (docker start)
    Start { targets: Vec<String> },
    /// Restart one or more containers (docker restart)
    Restart {
        targets: Vec<String>,
        #[arg(short = 't', long, default_value_t = 10)]
        grace: u64,
    },
    /// Block until one or more containers stop (docker wait)
    Wait { targets: Vec<String> },
    /// Rename a container (docker rename)
    Rename { target: String, new_name: String },
    /// Copy files between a container and the host (docker cp)
    Cp { src: String, dest: String },
    /// Display live resource-usage statistics (docker stats)
    Stats { target: Option<String> },
    /// Display the running processes of a container (docker top)
    Top { target: String },
    /// Manage container networks (docker network)
    Network {
        #[command(subcommand)]
        subcmd: NetworkCmd,
    },
    /// Manage named volumes (docker volume)
    Volume {
        #[command(subcommand)]
        subcmd: VolumeCmd,
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
        /// Set a build-time ARG value (docker --build-arg, repeatable): NAME=VALUE.
        /// Overrides the ARG's default; an override with no matching ARG line is
        /// ignored (Docker behavior).
        #[arg(long = "build-arg", value_name = "NAME=VALUE")]
        build_arg: Vec<String>,
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
    /// Generate an OS-supervisor unit (launchd/systemd) for a restart policy — no daemon of ours.
    Supervise {
        #[command(subcommand)]
        subcmd: SuperviseCmd,
    },
    /// Head-to-head benchmark vs Docker/OrbStack/Apple container on identical workloads.
    BenchCompare {
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "docker,orbstack,container"
        )]
        vs: Vec<String>,
        #[arg(long, default_value = "all")]
        workload: String,
        #[arg(long)]
        json: bool,
    },
    /// [internal] Supervise a detached run (hidden)
    #[command(name = "__supervise", hide = true)]
    SuperviseDetached { dir: String },
    /// [internal] Supervise a compose stack (hidden)
    #[command(name = "__compose-supervise", hide = true)]
    ComposeSupervisor { stack_dir: String },
}
