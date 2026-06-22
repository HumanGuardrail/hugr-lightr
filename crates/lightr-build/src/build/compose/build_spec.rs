//! WP-E: the serde model for a service's compose `build:` key + its lowered
//! runtime form.
//!
//! Docker accepts TWO shapes for `build:`:
//!   * SHORT — a bare string that IS the build context directory
//!     (`build: ./app`), with the default `Dockerfile` inside it.
//!   * LONG — a mapping (`build: { context, dockerfile, args, target }`).
//!
//! [`BuildSpec`] is untagged so a YAML scalar matches `Short` and a YAML map
//! matches `Long`. The serde layer is intentionally LENIENT (unknown keys
//! ignored, every field optional) — consistent with the rest of `spec.rs`.
//!
//! [`ServiceBuild`] is the LOWERED runtime form (`lower.rs` resolves the context
//! against the compose file's directory): a normalized `(context, dockerfile,
//! args, target)` the up-path feeds straight into the frozen build entrypoint
//! `build_target` (WP-C). It carries no `Value`, so the runtime model stays
//! serde-free.
use indexmap::IndexMap;
use serde::Deserialize;
use serde_yaml::Value;

/// A service's `build:` key — short string form OR the long mapping form.
/// Untagged: a YAML scalar deserializes to [`BuildSpec::Short`]; a YAML mapping
/// deserializes to [`BuildSpec::Long`].
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BuildSpec {
    /// `build: <context dir>` — the string IS the context directory; the
    /// Dockerfile is the default `Dockerfile` inside it.
    Short(String),
    /// `build: { context, dockerfile, args, target }`.
    Long(BuildLong),
}

/// The long `build:` mapping. Only the keys WP-E acts on are modeled; other
/// compose build keys (`cache_from`, `labels`, `network`, ...) are accepted and
/// ignored (house leniency — matches the rest of the compose spec model).
#[derive(Debug, Default, Deserialize)]
pub struct BuildLong {
    /// The build context directory. REQUIRED by the compose spec for the long
    /// form; an absent/empty context is fail-closed at lowering time.
    #[serde(default)]
    pub context: Option<String>,
    /// Path to the Dockerfile (relative to `context`, Docker's rule). Absent ⇒
    /// `<context>/Dockerfile`.
    #[serde(default)]
    pub dockerfile: Option<String>,
    /// Build-time `ARG` overrides. Docker accepts the MAP form (`KEY: value`) OR
    /// the LIST form (`["KEY=value", "KEY"]`); [`BuildArgs`] models both. A bare
    /// `KEY` (no value) passes the value through from the process environment.
    #[serde(default)]
    pub args: Option<BuildArgs>,
    /// `--target <stage>`: stop the multi-stage build at this named stage. Absent
    /// ⇒ the final stage (Docker default). Fed straight to `build_target`.
    #[serde(default)]
    pub target: Option<String>,
}

/// Build-args: the compose `args:` block, MAP form (`KEY: value`) or LIST form
/// (`["KEY=value", "KEY"]`). Untagged so both shapes parse.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BuildArgs {
    /// `args: { KEY: value }` — a null/absent value passes through the env.
    Map(IndexMap<String, Option<Value>>),
    /// `args: ["KEY=value", "KEY"]` — a bare `KEY` passes through the env.
    List(Vec<String>),
}

/// The LOWERED runtime form of a service's `build:` (see module docs). Produced
/// by `lower.rs` (which resolves the context against the compose file's dir) and
/// consumed by the up-path, which feeds it to `build_target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceBuild {
    /// The resolved build context directory (absolute when a base dir was known
    /// at lowering, else as-declared — see `lower.rs`).
    pub context: String,
    /// Path to the Dockerfile relative to `context` (default `Dockerfile`).
    pub dockerfile: String,
    /// Build-arg `(KEY, value)` overrides in declaration order. A bare `KEY`
    /// (env passthrough) is resolved here so the up-path needs no env access.
    pub args: Vec<(String, String)>,
    /// `--target <stage>`, when set.
    pub target: Option<String>,
}
