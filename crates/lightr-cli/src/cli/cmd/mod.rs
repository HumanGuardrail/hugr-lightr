//! CLI enum definitions — clap `Parser`/`Subcommand`/`ValueEnum` derives.
//! PURE MOVE from cmd.rs: every attribute and doc-comment preserved verbatim.

use clap::{Parser, Subcommand};

use crate::cli::version::{AFTER_HELP, LIGHTR_VERSION};

mod subcommands;
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
// argv parse) and immediately matched. Adding the Phase-1 `-p/--publish`
// `Vec<String>` to `Run` tips the largest variant just past clippy's 200-byte
// default. Boxing a clap field to satisfy a memory-layout lint on a
// once-per-process value is non-idiomatic and would distort the parse surface,
// so we allow the lint here rather than indirect a field.
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
    Run {
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(long)]
        input: Vec<String>,
        #[arg(long)]
        env: Vec<String>,
        #[arg(short = 'd', long)]
        detach: bool,
        /// Publish a container port to the host (Docker-style, repeatable):
        /// HOST:CONTAINER. Requires -d; native detached path only (Phase 1).
        #[arg(short = 'p', long = "publish", value_name = "HOST:CONTAINER")]
        publish: Vec<String>,
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
        /// Memory cap (Docker-style: 512m, 1g, 2048k, or bare bytes) — F-203
        #[arg(long, value_name = "SIZE")]
        memory: Option<String>,
        /// CPU cap as a core count (0.5, 1, 1.5) — F-203
        #[arg(long, value_name = "N")]
        cpus: Option<String>,
        /// Inject a store-backed secret file (repeatable): NAME=REF — F-309
        #[arg(long, value_name = "NAME=REF")]
        secret: Vec<String>,
        /// Inject a store-backed config file (repeatable): NAME=REF — F-309
        #[arg(long, value_name = "NAME=REF")]
        config: Vec<String>,
        /// Healthcheck command (probed when detached) — F-309
        #[arg(long, value_name = "CMD")]
        health_cmd: Option<String>,
        /// Healthcheck interval in seconds (docker --health-interval) — F-309
        #[arg(long, default_value_t = 30)]
        health_interval: u64,
        /// Healthcheck per-probe timeout in seconds (docker --health-timeout) — WP-RC-4
        #[arg(long, default_value_t = 30)]
        health_timeout: u64,
        /// Grace window after start before failures count (docker
        /// --health-start-period), in seconds — WP-RC-4
        #[arg(long, default_value_t = 0)]
        health_start_period: u64,
        /// Healthcheck retries before Unhealthy (docker --health-retries) — F-309
        #[arg(long, default_value_t = 3)]
        health_retries: u32,
        /// Disable any healthcheck for this run (docker --no-healthcheck) — WP-RC-4
        #[arg(long)]
        no_healthcheck: bool,
        // ── Docker-parity run flags (CLI-surface freeze; behavior per WP-RUNFLAGS) ──
        /// Assign a name to the container (docker --name)
        #[arg(long)]
        name: Option<String>,
        /// Remove the container when it exits (docker --rm)
        #[arg(long = "rm")]
        rm: bool,
        /// Working directory inside the container (docker -w/--workdir)
        #[arg(short = 'w', long)]
        workdir: Option<String>,
        /// User[:group] to run as (docker -u/--user)
        #[arg(short = 'u', long)]
        user: Option<String>,
        /// Set environment variables (docker -e/--env, repeatable). The long
        /// `--env` already binds the memo env-KEYS list above; this adds only
        /// the docker short `-e` (KEY=VAL). WP-RUNFLAGS owns reconciling the
        /// two `--env` grammars. Flagged in the return card.
        #[arg(short = 'e', value_name = "KEY=VAL")]
        env_set: Vec<String>,
        /// Read environment variables from a file (docker --env-file)
        #[arg(long)]
        env_file: Option<String>,
        /// Set metadata labels (docker --label, repeatable)
        #[arg(long, value_name = "KEY=VAL")]
        label: Vec<String>,
        /// Override the image entrypoint (docker --entrypoint)
        #[arg(long)]
        entrypoint: Option<String>,
        /// Container hostname (docker --hostname)
        #[arg(long)]
        hostname: Option<String>,
        /// Restart policy (docker --restart)
        #[arg(long)]
        restart: Option<String>,
        /// Signal to stop the container (docker --stop-signal). Numeric or a
        /// portable name (HUP/INT/QUIT/KILL/TERM). Default SIGTERM.
        #[arg(long, value_name = "SIG")]
        stop_signal: Option<String>,
        /// Connect to a network (docker --network)
        #[arg(long)]
        network: Option<String>,
        /// Network-scoped alias (docker --network-alias, repeatable)
        #[arg(long)]
        network_alias: Vec<String>,
        /// Add a custom host-to-IP mapping (docker --add-host, repeatable)
        #[arg(long, value_name = "HOST:IP")]
        add_host: Vec<String>,
        /// Set custom DNS servers (docker --dns, repeatable)
        #[arg(long)]
        dns: Vec<String>,
        /// Bind mount a volume (docker -v/--volume, repeatable)
        #[arg(short = 'v', long = "volume", value_name = "SRC:DST")]
        volume: Vec<String>,
        // NOTE: docker's `--mount` is intentionally NOT re-added here — the Run
        // variant already owns `--mount` (lightr REF:TARGET, field `mount`
        // above). Re-declaring `long = "mount"` would be a clap conflict. The
        // docker `--mount` type=... syntax is deferred to WP-RUNFLAGS, which
        // owns reconciling the two grammars. Flagged in the return card.
        /// Mount a tmpfs directory (docker --tmpfs, repeatable)
        #[arg(long)]
        tmpfs: Vec<String>,
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
