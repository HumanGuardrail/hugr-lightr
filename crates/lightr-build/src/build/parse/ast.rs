//! Dockerfile AST types — the full 18-instruction structured form (WP-DF-01).
//!
//! This module owns the *shape* of a parsed Dockerfile. Parsing logic lives in
//! `super` (`parse/mod.rs`). No `${VAR}` interpolation happens here or in the
//! parser — the raw text is captured faithfully; WP-DF-02 (R-VARENG) consumes
//! this AST and interpolates later.

/// Exec form (JSON array `["a","b"]`) vs shell form (raw command string).
///
/// Docker distinguishes these for RUN / CMD / ENTRYPOINT. Exec form is run
/// directly; shell form is wrapped by the image's SHELL (default `/bin/sh -c`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CmdForm {
    /// JSON-array exec form: argv passed verbatim, no shell wrapping.
    Exec(Vec<String>),
    /// Shell form: the raw command string (wrapped by SHELL at exec time).
    Shell(String),
}

/// HEALTHCHECK options (`--interval`, `--timeout`, `--start-period`,
/// `--start-interval`, `--retries`), parsed into fields (NOT raw strings).
/// Durations/counts are kept as their raw token text (faithful, un-interpreted).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HealthcheckOpts {
    pub interval: Option<String>,
    pub timeout: Option<String>,
    pub start_period: Option<String>,
    pub start_interval: Option<String>,
    pub retries: Option<String>,
}

/// HEALTHCHECK body: either `NONE` or a `CMD <cmd>` with its options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Healthcheck {
    /// `HEALTHCHECK NONE` — disable any inherited healthcheck.
    None,
    /// `HEALTHCHECK [opts] CMD <command>`.
    Cmd { opts: HealthcheckOpts, cmd: CmdForm },
}

/// Parser directives recognized at the very top of a Dockerfile
/// (`# syntax=...`, `# escape=...`). Captured separately from instructions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directives {
    /// `# syntax=<frontend image ref>` — BuildKit frontend selector.
    pub syntax: Option<String>,
    /// `# escape=<char>` — line-continuation escape char (`\` default, `` ` ``).
    pub escape: Option<char>,
}

/// A fully structured Dockerfile instruction. All 18 instructions plus their
/// structured flags. Newly-recognized instructions parse into the AST even
/// where execution is not yet implemented (DF-02..15 implement execution).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instr {
    /// `FROM [--platform=<p>] <image_ref> [AS <stage>]`.
    From {
        image_ref: String,
        platform: Option<String>,
        stage: Option<String>,
    },
    /// `RUN <cmd>` — `argv` is the resolved exec argv (shell-wrapped if shell
    /// form), `form` is the structured exec-vs-shell distinction.
    Run { argv: Vec<String>, form: CmdForm },
    /// `CMD <cmd>` — default argv. `argv`/`form` as in `Run`.
    Cmd { argv: Vec<String>, form: CmdForm },
    /// `ENTRYPOINT <cmd>`.
    Entrypoint { argv: Vec<String>, form: CmdForm },
    /// `LABEL k=v` (one pair per parsed instruction occurrence).
    Label { key: String, val: String },
    /// `EXPOSE <port>[/<proto>] ...` — raw port spec tokens, faithful.
    Expose { ports: Vec<String> },
    /// `ENV k=v` (one pair per parsed instruction occurrence).
    Env { key: String, val: String },
    /// `ADD [--chown=] [--chmod=] <src>... <dest>`.
    Add {
        src: Vec<String>,
        dest: String,
        chown: Option<String>,
        chmod: Option<String>,
    },
    /// `COPY [--from=] [--chown=] [--chmod=] <src>... <dest>`.
    Copy {
        src: Vec<String>,
        dest: String,
        from: Option<String>,
        chown: Option<String>,
        chmod: Option<String>,
    },
    /// `VOLUME <path>...` (or JSON array form) — raw paths.
    Volume { paths: Vec<String> },
    /// `USER <user>[:<group>]`.
    User { user: String },
    /// `WORKDIR <path>`.
    Workdir { path: String },
    /// `ARG <name>[=<default>]`.
    Arg {
        name: String,
        default: Option<String>,
    },
    /// `ONBUILD <instruction>` — the deferred instruction, parsed recursively.
    Onbuild { instr: Box<Instr> },
    /// `STOPSIGNAL <signal>`.
    Stopsignal { signal: String },
    /// `HEALTHCHECK ...`.
    Healthcheck { check: Healthcheck },
    /// `SHELL ["exe","arg"]` — must be JSON exec form per Docker.
    Shell { shell: Vec<String> },
}

/// A parsed Dockerfile step: the structured instruction plus its raw,
/// continuation-joined source text (the canonical text the memo key hashes).
#[derive(Clone, Debug)]
pub struct BuildStep {
    pub instr: Instr,
    pub raw: String,
}
