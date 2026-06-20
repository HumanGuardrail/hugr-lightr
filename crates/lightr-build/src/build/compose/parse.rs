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
        });
    }
    let spec: ComposeSpec = serde_yaml::from_str(yaml)
        .map_err(|e| LightrError::InvalidManifest(format!("compose parse error: {e}")))?;
    lower(spec)
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
