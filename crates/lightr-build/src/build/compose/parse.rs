//! Docker-compose YAML parser.
//!
//! Deserializes the compose file into the serde compose-spec model
//! (`spec::ComposeSpec`) via `serde_yaml`, then LOWERS it to the runtime
//! `Compose` type (see `lower.rs`) so up/down/supervise are unaffected.
//!
//! LENIENT: unknown keys are ignored (no `deny_unknown_fields`). Polymorphic
//! Docker forms (environment list-or-map, command string-or-list, ...) are
//! handled by the model. Fail-closed on malformed YAML.
use lightr_core::{LightrError, Result};

use super::super::vars::VarScope;
use super::interp::interpolate_compose;
use super::lower::lower;
use super::model::Compose;
use super::spec::ComposeSpec;

/// Parse a docker-compose YAML file into the runtime `Compose`.
///
/// Supported structure (compose-spec subset):
/// ```yaml
/// services:
///   <name>:
///     image: <ref>
///     command: "string" | ["a","b"]
///     ports:
///       - "H:C"
///     environment:        # list OR map form
///       - K=V
///       K: V
///     x-lightr-eager: true
/// ```
/// Unknown keys are silently ignored. Returns `InvalidManifest` on malformed
/// YAML or on a structurally-invalid value (e.g. a non-numeric port).
pub fn parse_compose(yaml: &str) -> Result<Compose> {
    // An empty document deserializes to `null`; treat it as an empty compose.
    if yaml.trim().is_empty() {
        return Ok(Compose {
            services: Vec::new(),
            secret_sources: Vec::new(),
            config_sources: Vec::new(),
        });
    }
    let spec: ComposeSpec = serde_yaml::from_str(yaml)
        .map_err(|e| LightrError::InvalidManifest(format!("compose parse error: {e}")))?;
    lower(spec)
}

/// Parse a docker-compose YAML file, INTERPOLATING `${VAR}` references against
/// `scope` first (compose interpolates the whole document before parsing).
///
/// Equivalent to [`parse_compose`] but with compose-spec variable substitution
/// applied to the raw text: `${VAR}` / `${VAR:-default}` / `${VAR:?err}` /
/// `$VAR`, with `$$` → literal `$`. `scope` carries process-env-over-`.env`
/// precedence (build it via [`super::interp::scope_from_project_dir`] at the
/// CLI call site, or directly in tests to stay parallel-safe). Fail-closed: a
/// triggered `${VAR:?msg}` or an unclosed `${` is an honest error.
///
/// Behavior-preserving: a document with no `${...}` interpolates to itself and
/// parses identically to [`parse_compose`].
pub fn parse_compose_with_scope(yaml: &str, scope: &VarScope) -> Result<Compose> {
    let interpolated = interpolate_compose(yaml, scope)?;
    parse_compose(&interpolated)
}

/// CMP-P1-PROJECT: extract the compose file's top-level `name:` field, if any.
///
/// Used by project-name resolution (precedence rung 3). Returns `None` for an
/// empty document or a file with no `name:`. Lenient like [`parse_compose`]:
/// unknown keys are ignored. A malformed document is an honest error so the
/// caller fails closed rather than silently dropping the field.
pub fn parse_compose_project_name(yaml: &str) -> Result<Option<String>> {
    if yaml.trim().is_empty() {
        return Ok(None);
    }
    let spec: ComposeSpec = serde_yaml::from_str(yaml)
        .map_err(|e| LightrError::InvalidManifest(format!("compose parse error: {e}")))?;
    Ok(spec.name)
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
