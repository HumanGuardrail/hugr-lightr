//! `RunArgs` — the flattened flag surface of the `run` subcommand.
//!
//! SKELETON-FREEZE: these fields were lifted VERBATIM (every `#[arg(...)]`
//! attribute and doc-comment preserved) out of the `Cmd::Run { … }` enum
//! variant in `cmd/mod.rs`. Clap `#[derive(Args)]` re-derives the IDENTICAL
//! flag surface (same long/short names, defaults, value names) — the CLI is
//! byte-for-byte the same; only the in-tree representation changed from a
//! struct-variant to `Run(RunArgs)`. Future run-flag WPs edit THIS struct,
//! not the `Cmd` enum (which is a chronic 400-cap overflow point).
//!
//! Behavior-preserving: NO semantic change, NO new flag.

use clap::Args;

#[derive(Args)]
pub(crate) struct RunArgs {
    #[arg(long, default_value = ".")]
    pub(crate) dir: String,
    #[arg(long)]
    pub(crate) input: Vec<String>,
    #[arg(long)]
    pub(crate) env: Vec<String>,
    #[arg(short = 'd', long)]
    pub(crate) detach: bool,
    /// Publish a container port to the host (Docker-style, repeatable):
    /// HOST:CONTAINER. Requires -d; native detached path only (Phase 1).
    #[arg(short = 'p', long = "publish", value_name = "HOST:CONTAINER")]
    pub(crate) publish: Vec<String>,
    #[arg(long, value_name = "REF:TARGET")]
    pub(crate) mount: Vec<String>,
    /// Engine to use: native (default), ns, vz
    #[arg(long, default_value = "native", value_name = "ENGINE")]
    pub(crate) engine: String,
    /// Hydrate a ref CoW into a temp dir and hand it to the engine as rootfs
    #[arg(long, value_name = "REF")]
    pub(crate) rootfs: Option<String>,
    /// Process-tree memoization (opt-in; honest fallback to whole-run memo)
    #[arg(long)]
    pub(crate) deep_memo: bool,
    /// Memory cap (Docker-style: 512m, 1g, 2048k, or bare bytes) — F-203
    #[arg(long, value_name = "SIZE")]
    pub(crate) memory: Option<String>,
    /// CPU cap as a core count (0.5, 1, 1.5) — F-203
    #[arg(long, value_name = "N")]
    pub(crate) cpus: Option<String>,
    /// Inject a store-backed secret file (repeatable): NAME=REF — F-309
    #[arg(long, value_name = "NAME=REF")]
    pub(crate) secret: Vec<String>,
    /// Inject a store-backed config file (repeatable): NAME=REF — F-309
    #[arg(long, value_name = "NAME=REF")]
    pub(crate) config: Vec<String>,
    /// Healthcheck command (probed when detached) — F-309
    #[arg(long, value_name = "CMD")]
    pub(crate) health_cmd: Option<String>,
    /// Healthcheck interval in seconds (docker --health-interval) — F-309
    #[arg(long, default_value_t = 30)]
    pub(crate) health_interval: u64,
    /// Healthcheck per-probe timeout in seconds (docker --health-timeout) — WP-RC-4
    #[arg(long, default_value_t = 30)]
    pub(crate) health_timeout: u64,
    /// Grace window after start before failures count (docker
    /// --health-start-period), in seconds — WP-RC-4
    #[arg(long, default_value_t = 0)]
    pub(crate) health_start_period: u64,
    /// Healthcheck retries before Unhealthy (docker --health-retries) — F-309
    #[arg(long, default_value_t = 3)]
    pub(crate) health_retries: u32,
    /// Disable any healthcheck for this run (docker --no-healthcheck) — WP-RC-4
    #[arg(long)]
    pub(crate) no_healthcheck: bool,
    // ── Docker-parity run flags (CLI-surface freeze; behavior per WP-RUNFLAGS) ──
    /// Assign a name to the container (docker --name)
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Remove the container when it exits (docker --rm)
    #[arg(long = "rm")]
    pub(crate) rm: bool,
    /// Working directory inside the container (docker -w/--workdir)
    #[arg(short = 'w', long)]
    pub(crate) workdir: Option<String>,
    /// User[:group] to run as (docker -u/--user)
    #[arg(short = 'u', long)]
    pub(crate) user: Option<String>,
    /// Set environment variables (docker -e/--env, repeatable). The long
    /// `--env` already binds the memo env-KEYS list above; this adds only
    /// the docker short `-e` (KEY=VAL). WP-RUNFLAGS owns reconciling the
    /// two `--env` grammars. Flagged in the return card.
    #[arg(short = 'e', value_name = "KEY=VAL")]
    pub(crate) env_set: Vec<String>,
    /// Read environment variables from a file (docker --env-file)
    #[arg(long)]
    pub(crate) env_file: Option<String>,
    /// Set metadata labels (docker -l/--label, repeatable)
    #[arg(short = 'l', long, value_name = "KEY=VAL")]
    pub(crate) label: Vec<String>,
    /// Override the image entrypoint (docker --entrypoint)
    #[arg(long)]
    pub(crate) entrypoint: Option<String>,
    /// Container hostname (docker --hostname)
    #[arg(long)]
    pub(crate) hostname: Option<String>,
    /// Restart policy (docker --restart)
    #[arg(long)]
    pub(crate) restart: Option<String>,
    /// Signal to stop the container (docker --stop-signal). Numeric or a
    /// portable name (HUP/INT/QUIT/KILL/TERM). Default SIGTERM.
    #[arg(long, value_name = "SIG")]
    pub(crate) stop_signal: Option<String>,
    /// Connect to a network (docker --network)
    #[arg(long)]
    pub(crate) network: Option<String>,
    /// Network-scoped alias (docker --network-alias, repeatable)
    #[arg(long)]
    pub(crate) network_alias: Vec<String>,
    /// Add a custom host-to-IP mapping (docker --add-host, repeatable)
    #[arg(long, value_name = "HOST:IP")]
    pub(crate) add_host: Vec<String>,
    /// Set custom DNS servers (docker --dns, repeatable)
    #[arg(long)]
    pub(crate) dns: Vec<String>,
    /// Bind mount a volume (docker -v/--volume, repeatable)
    #[arg(short = 'v', long = "volume", value_name = "SRC:DST")]
    pub(crate) volume: Vec<String>,
    // NOTE: docker's `--mount` is intentionally NOT re-added here — the Run
    // variant already owns `--mount` (lightr REF:TARGET, field `mount`
    // above). Re-declaring `long = "mount"` would be a clap conflict. The
    // docker `--mount` type=... syntax is deferred to WP-RUNFLAGS, which
    // owns reconciling the two grammars. Flagged in the return card.
    /// Mount a tmpfs directory (docker --tmpfs, repeatable)
    #[arg(long)]
    pub(crate) tmpfs: Vec<String>,
    // ── WP-CLI-TRIO / RC-FLAGS: 11 run-config flags, WIRED to RunSpec carry-
    // fields (off the WP-RUNFLAGS stub guard). RUNTIME-ONLY (never keyed).
    // `hostname` + `label` already declared above; the rest are added here. ─────
    /// Add a Linux capability (docker --cap-add, repeatable)
    #[arg(long, value_name = "CAP")]
    pub(crate) cap_add: Vec<String>,
    /// Drop a Linux capability (docker --cap-drop, repeatable)
    #[arg(long, value_name = "CAP")]
    pub(crate) cap_drop: Vec<String>,
    /// Give extended privileges to the container (docker --privileged)
    #[arg(long)]
    pub(crate) privileged: bool,
    /// Allocate a pseudo-TTY (docker -t/--tty)
    #[arg(short = 't', long)]
    pub(crate) tty: bool,
    /// Run an init inside the container as PID 1 (docker --init)
    #[arg(long)]
    pub(crate) init: bool,
    /// Mount the container's root filesystem read-only (docker --read-only)
    #[arg(long)]
    pub(crate) read_only: bool,
    /// Tune the host OOM killer preference (docker --oom-score-adj)
    #[arg(long, value_name = "N", allow_hyphen_values = true)]
    pub(crate) oom_score_adj: Option<i32>,
    /// Tune the container pids limit (docker --pids-limit)
    #[arg(long, value_name = "N")]
    pub(crate) pids_limit: Option<i64>,
    /// Size of /dev/shm (docker --shm-size: 64m, 1g, or bare bytes)
    #[arg(long, value_name = "SIZE")]
    pub(crate) shm_size: Option<String>,
    #[arg(last = true, required = true)]
    pub(crate) command: Vec<String>,
}
