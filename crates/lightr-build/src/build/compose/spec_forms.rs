//! The polymorphic value-form enums docker-compose allows for scalar/list/map
//! fields (`command`/`entrypoint` as string-or-list, `extra_hosts` as
//! list-or-map, `environment` as list-or-map). Split out of `spec.rs` for
//! godfile headroom (`spec.rs` sat at the 400-LOC ceiling); `spec.rs`
//! re-exports every type here so `super::spec::{StringOrList, ...}` imports
//! resolve unchanged.
use indexmap::IndexMap;
use serde::Deserialize;

/// A field Docker accepts as either a bare string or a list of strings
/// (`command`, `entrypoint`, `env_file`, ...).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum StringOrList {
    String(String),
    List(Vec<String>),
}

/// WP-A: the `extra_hosts` field. Docker accepts BOTH the LIST form
/// (`["host:ip", "other:ip"]`) AND the MAP form (`{host: ip, other: ip}`).
/// Untagged so a YAML sequence matches `List` and a YAML mapping matches `Map`;
/// both lower to the `"host:ip"` strings `RunSpec.add_host` expects (see
/// `lower_net::lower_extra_hosts`). Map values are scalars (IPs); modeled as
/// strings (compose IPs are always quotable scalars).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ExtraHosts {
    /// List form: each entry is already a `"host:ip"` string.
    List(Vec<String>),
    /// Map form: `host -> ip`, joined to `"host:ip"` at lowering time.
    Map(IndexMap<String, String>),
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
