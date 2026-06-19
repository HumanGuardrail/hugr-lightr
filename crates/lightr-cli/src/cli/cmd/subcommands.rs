//! Sub-command enums — clap `Subcommand`/`ValueEnum` derives.
//! PURE MOVE from cmd.rs: every attribute and doc-comment preserved verbatim.

use clap::{Subcommand, ValueEnum};

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
    /// Push a stored ref to a registry as a synthesized single-layer OCI image
    Push {
        /// Stored ref to push (e.g. @me/img)
        store_ref: String,
        /// Target registry reference (e.g. ghcr.io/owner/repo:tag)
        target: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────────
// SuperviseCmd sub-enum (F-308 — OS-supervisor unit generation)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum SuperviseCmd {
    /// Generate + write an OS-supervisor unit for a restart policy
    Install {
        #[arg(long)]
        name: String,
        /// Restart policy: no | always | on-failure[:N] | unless-stopped
        #[arg(long, default_value = "always")]
        restart: String,
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Remove a previously installed unit
    Uninstall {
        #[arg(long)]
        name: String,
    },
    /// List installed units
    List,
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
