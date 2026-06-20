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
    #[serde(default)]
    pub networks: Option<Value>,
    #[serde(default)]
    pub depends_on: Option<Value>,
    #[serde(default)]
    pub healthcheck: Option<Healthcheck>,
    #[serde(default)]
    pub restart: Option<String>,
    #[serde(default)]
    pub deploy: Option<Value>,
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

/// Service healthcheck. `test` is string-or-list (`["CMD", ...]` or a shell
/// string); `interval`/`retries` are optional with Docker-faithful defaults
/// applied at lowering time.
#[derive(Debug, Default, Deserialize)]
pub struct Healthcheck {
    #[serde(default)]
    pub test: Option<StringOrList>,
    /// Legacy alias the hand-rolled parser accepted.
    #[serde(default)]
    pub cmd: Option<StringOrList>,
    #[serde(default)]
    pub interval: Option<Value>,
    #[serde(default)]
    pub retries: Option<u32>,
}
