//! Serde compose-spec model: a faithful `serde_yaml` deserialization of the
//! docker-compose-spec services/volumes/networks maps.
//!
//! This layer is intentionally LENIENT: unknown keys are ignored (NO
//! `#[serde(deny_unknown_fields)]` — unknown-key validation is a separate
//! later WP, D1), and the polymorphic forms Docker allows (environment as a
//! list OR a map, command/entrypoint as a string OR a list, etc.) are modeled
//! with helper enums. Every field is `#[serde(default)]`/optional so partial
//! files parse. CMP-P1/P2 consume the richer fields; today only the subset
//! the `Compose` runtime type carries is lowered (see `lower.rs`).
use indexmap::IndexMap;
use serde::Deserialize;
use serde_yaml::Value;

/// Top-level compose file.
#[derive(Debug, Default, Deserialize)]
pub struct ComposeSpec {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// `IndexMap` preserves declaration order of services (Docker is order-
    /// stable for the user-facing list).
    #[serde(default)]
    pub services: IndexMap<String, ServiceDef>,
    #[serde(default)]
    pub volumes: IndexMap<String, Value>,
    #[serde(default)]
    pub networks: IndexMap<String, Value>,
    /// SKELETON-FREEZE: top-level `secrets:` block (source/file/external/...).
    /// Data-only; the feature WP lowers it. Kept as raw `Value` per entry so the
    /// later WP owns the source/external grammar.
    #[serde(default)]
    pub secrets: IndexMap<String, Value>,
    /// SKELETON-FREEZE: top-level `configs:` block. Data-only; lowered by the
    /// feature WP.
    #[serde(default)]
    pub configs: IndexMap<String, Value>,
    /// SKELETON-FREEZE: top-level `profiles:` list. Data-only; the feature WP
    /// applies profile activation.
    #[serde(default)]
    pub profiles: Vec<String>,
}

/// A single service entry under `services:`.
#[derive(Debug, Default, Deserialize)]
pub struct ServiceDef {
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub build: Option<Value>,
    #[serde(default)]
    pub command: Option<StringOrList>,
    #[serde(default)]
    pub entrypoint: Option<StringOrList>,
    #[serde(default)]
    pub environment: Option<Environment>,
    #[serde(default)]
    pub env_file: Option<StringOrList>,
    /// CMP-P0-PORTS-FULL: the full compose `ports` grammar — each entry is a
    /// short scalar (`"8080:80"`, `"80"`, `"127.0.0.1:8080:80/udp"`, ranges) or
    /// the long mapping form. Parsed/lowered by `ports.rs`/`lower.rs`.
    #[serde(default)]
    pub ports: Vec<PortSpec>,
    #[serde(default)]
    pub expose: Vec<Value>,
    #[serde(default)]
    pub volumes: Vec<Value>,
    /// SKELETON-FREEZE: service `networks` — short list (`["frontend"]`) OR map
    /// with per-network `aliases`/`ipv4_address`/.... Typed [`ServiceNetworks`];
    /// data-only. (Replaces the prior raw-`Value` shape — same YAML parses.)
    #[serde(default)]
    pub networks: Option<ServiceNetworks>,
    /// SKELETON-FREEZE: short list (`["db", "redis"]`) OR long map with
    /// per-dependency `condition`. Modeled as a typed [`DependsOn`] so the
    /// feature WP transcribes the condition; data-only here.
    #[serde(default)]
    pub depends_on: Option<DependsOn>,
    #[serde(default)]
    pub healthcheck: Option<Healthcheck>,
    #[serde(default)]
    pub restart: Option<String>,
    /// SKELETON-FREEZE: the `deploy` block (replicas, resources.limits,
    /// restart_policy). Typed [`Deploy`]; data-only — the feature WP lowers it.
    #[serde(default)]
    pub deploy: Option<Deploy>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub labels: Option<Value>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub container_name: Option<String>,
    /// SKELETON-FREEZE: `extends` (from another file/service). Data-only —
    /// modeled as raw `Value`; the feature WP owns the file/service grammar.
    #[serde(default)]
    pub extends: Option<Value>,
    /// SKELETON-FREEZE: extra `/etc/hosts` entries (`["host:ip", ...]` or a map).
    #[serde(default)]
    pub extra_hosts: Option<StringOrList>,
    /// SKELETON-FREEZE: graceful-stop window (compose duration string).
    #[serde(default)]
    pub stop_grace_period: Option<String>,
    /// SKELETON-FREEZE: signal used to stop the container (e.g. `SIGTERM`).
    #[serde(default)]
    pub stop_signal: Option<String>,
    /// SKELETON-FREEZE: run an init process (PID 1 reaper) inside the container.
    #[serde(default)]
    pub init: Option<bool>,
    /// SKELETON-FREEZE: allocate a TTY.
    #[serde(default)]
    pub tty: Option<bool>,
    /// SKELETON-FREEZE: capabilities to add.
    #[serde(default)]
    pub cap_add: Vec<String>,
    /// SKELETON-FREEZE: capabilities to drop.
    #[serde(default)]
    pub cap_drop: Vec<String>,
    /// SKELETON-FREEZE: run the container in privileged mode.
    #[serde(default)]
    pub privileged: Option<bool>,
    /// Lightr extension preserved from the legacy parser: eager-start a
    /// service rather than binding lazily.
    #[serde(default, rename = "x-lightr-eager")]
    pub x_lightr_eager: Option<bool>,
    /// Lightr extension preserved from the legacy parser: store-backed
    /// secrets as `name=ref` strings (compose-spec `secrets` is richer; the
    /// legacy lowering treats the list-of-`name=ref` form).
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Lightr extension preserved from the legacy parser: store-backed
    /// configs as `name=ref` strings.
    #[serde(default)]
    pub configs: Vec<String>,
}

/// SKELETON-FREEZE: service `depends_on` — Docker accepts the short list form
/// (`["db", "redis"]`) OR the long map form keyed by service name with a
/// per-dependency `condition` (and optional `restart`/`required`). Untagged so
/// both shapes parse; data-only. The feature WP (CMP-P0-DEPENDS) lowers it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum DependsOn {
    /// Short form: a plain list of service names.
    List(Vec<String>),
    /// Long form: `name -> { condition, restart, required }`.
    Map(IndexMap<String, DependsOnEntry>),
}

/// SKELETON-FREEZE: a long-form `depends_on` entry. Data-only.
#[derive(Debug, Default, Deserialize)]
pub struct DependsOnEntry {
    /// `service_started` | `service_healthy` | `service_completed_successfully`.
    #[serde(default)]
    pub condition: Option<String>,
    /// Whether the dependency is restarted when it is updated.
    #[serde(default)]
    pub restart: Option<bool>,
    /// Whether the dependency is required (compose-spec `required`, default true).
    #[serde(default)]
    pub required: Option<bool>,
}

/// SKELETON-FREEZE: the `deploy` block. Only the fields the parity contract
/// names (replicas, resources.limits {cpus,memory}, restart_policy) are modeled;
/// other deploy keys are ignored (house leniency). Data-only; the feature WP
/// (CMP-P1-DEPLOY-RES) lowers it.
#[derive(Debug, Default, Deserialize)]
pub struct Deploy {
    #[serde(default)]
    pub replicas: Option<u32>,
    #[serde(default)]
    pub resources: Option<DeployResources>,
    #[serde(default)]
    pub restart_policy: Option<RestartPolicy>,
}

/// SKELETON-FREEZE: `deploy.resources` (limits / reservations). Data-only.
#[derive(Debug, Default, Deserialize)]
pub struct DeployResources {
    #[serde(default)]
    pub limits: Option<ResourceSpec>,
    #[serde(default)]
    pub reservations: Option<ResourceSpec>,
}

/// SKELETON-FREEZE: a `{cpus, memory}` resource spec. `cpus` is a string in the
/// compose spec (`"0.5"`); kept as a string to transcribe faithfully. Data-only.
#[derive(Debug, Default, Deserialize)]
pub struct ResourceSpec {
    #[serde(default)]
    pub cpus: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
}

/// SKELETON-FREEZE: `deploy.restart_policy`. Data-only.
#[derive(Debug, Default, Deserialize)]
pub struct RestartPolicy {
    /// `none` | `on-failure` | `any`.
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub delay: Option<String>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub window: Option<String>,
}

/// SKELETON-FREEZE: service `networks` — short list (`["frontend", "backend"]`)
/// OR a map keyed by network name with per-attachment options (`aliases`,
/// `ipv4_address`, ...). Untagged so both parse; data-only. The feature WP
/// (CMP-P1-NETWORKS) lowers it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ServiceNetworks {
    /// Short form: a plain list of network names.
    List(Vec<String>),
    /// Long form: `name -> attachment options` (a null value is allowed).
    Map(IndexMap<String, Option<NetworkAttachment>>),
}

/// SKELETON-FREEZE: per-network attachment options under the long `networks`
/// map. Only `aliases` is named by the contract; other keys are accepted and
/// ignored (leniency). Data-only.
#[derive(Debug, Default, Deserialize)]
pub struct NetworkAttachment {
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub ipv4_address: Option<String>,
    #[serde(default)]
    pub ipv6_address: Option<String>,
}

/// A single compose `ports` entry: Docker accepts BOTH the short scalar form
/// (a string `"8080:80"` or a bare port number) AND the long mapping form. The
/// untagged enum tries the long map first (a YAML mapping never matches the
/// scalar arm), then falls back to the short scalar (kept as a raw `Value` so
/// `ports.rs` owns the string grammar). Fail-closed parsing lives in `ports.rs`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PortSpec {
    /// Long mapping form: `{ target, published, protocol, host_ip, mode }`.
    Long(PortLong),
    /// Short scalar form: a string (`"8080:80"`, `"80"`, `".../udp"`, ranges)
    /// or a bare YAML number (`8080`).
    Short(Value),
}

/// The long compose `ports` mapping form. Only the fields Lightr lowers are
/// modeled; unknown keys (e.g. `app_protocol`) are ignored (house leniency).
/// `mode` is accepted but not yet acted on (Lightr publishes on loopback).
#[derive(Debug, Default, Deserialize)]
pub struct PortLong {
    /// Container port. REQUIRED by the compose spec; absence is fail-closed at
    /// lowering time.
    #[serde(default)]
    pub target: Option<u16>,
    /// Published host port. Absent ⇒ host auto-assigned.
    #[serde(default)]
    pub published: Option<u16>,
    /// `tcp` (default) or `udp`.
    #[serde(default)]
    pub protocol: Option<String>,
    /// Host bind address. Defaults to loopback (Lightr's publish model).
    #[serde(default)]
    pub host_ip: Option<String>,
    /// `host` | `ingress`. Accepted for spec-faithful parsing; not yet acted on.
    #[serde(default)]
    pub mode: Option<String>,
}

/// A field Docker accepts as either a bare string or a list of strings
/// (`command`, `entrypoint`, `env_file`, ...).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum StringOrList {
    String(String),
    List(Vec<String>),
}

/// `environment` accepts both the list form (`- FOO=bar`) and the map form
/// (`FOO: bar`). Map values may be scalars or null (Docker passes the host
/// value through for null) — modeled as `Option<String>`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Environment {
    List(Vec<String>),
    Map(IndexMap<String, Option<EnvScalar>>),
}

/// A scalar environment value: Docker coerces numbers/bools to strings.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum EnvScalar {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl EnvScalar {
    pub(crate) fn into_string(self) -> String {
        match self {
            EnvScalar::String(s) => s,
            EnvScalar::Int(n) => n.to_string(),
            EnvScalar::Float(f) => f.to_string(),
            EnvScalar::Bool(b) => b.to_string(),
        }
    }
}

/// Service healthcheck (CMP-P1-HEALTH-FULL — the full compose-spec form).
///
/// `test` is string-or-list:
///  * list `["CMD", "curl", ...]` (exec form) / `["CMD-SHELL", "curl ..."]`
///    (shell form) / `["NONE"]` (disable),
///  * string `"CMD-SHELL ..."` / a bare shell string / `"NONE"` (disable).
///
/// `interval`/`timeout`/`start_period` are compose duration strings (`30s`,
/// `1m30s`, a bare integer ⇒ seconds) parsed with `parse_duration_secs` at
/// lowering time; `retries` is a count. `disable: true` is the explicit
/// compose toggle that drops any healthcheck (== `test: NONE`). Every field is
/// optional with Docker-faithful defaults applied at lowering time.
#[derive(Debug, Default, Deserialize)]
pub struct Healthcheck {
    #[serde(default)]
    pub test: Option<StringOrList>,
    /// Legacy alias the hand-rolled parser accepted.
    #[serde(default)]
    pub cmd: Option<StringOrList>,
    #[serde(default)]
    pub interval: Option<Value>,
    /// Per-probe timeout (compose `timeout`). Lowered to the runtime
    /// `Healthcheck.timeout_s` (Docker default 30s when absent).
    #[serde(default)]
    pub timeout: Option<Value>,
    /// Grace window after start (compose `start_period`). Lowered to the runtime
    /// `Healthcheck.start_period_s` (Docker default 0s when absent).
    #[serde(default)]
    pub start_period: Option<Value>,
    #[serde(default)]
    pub retries: Option<u32>,
    /// Compose `disable: true` ⇒ no healthcheck (equivalent to `test: NONE`).
    #[serde(default)]
    pub disable: Option<bool>,
}

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;
