//! Compose override deep-merge (docker-compose `compose.override.yaml`
//! semantics).
//!
//! Docker-compose auto-applies an override file beside the base compose file and
//! deep-merges it OVER the base before the document is interpreted. This module
//! implements that engine at the `serde_yaml::Value` level (BEFORE serde-
//! deserializing into the spec model), then routes the merged text through the
//! existing [`parse_compose_with_scope`] so interpolation + lowering are
//! unchanged.
//!
//! Merge rules (docker-compose's documented behavior):
//! - **maps** merge recursively (override keys win; base-only keys are kept);
//! - **sequences** are REPLACED wholesale by the override (compose's documented
//!   list behavior is replace, not append — kept simple + documented here);
//! - **scalars** (and any type mismatch, e.g. override map over base scalar) are
//!   overridden by the override value.
//!
//! Behavior-preserving: with `override_yaml == None` this is a passthrough to
//! [`parse_compose_with_scope`] over the base text, byte-for-byte identical to
//! today's path.
//!
//! The auto-detected override filenames (docker-compose order):
//! `compose.override.yaml`, `compose.override.yml`, `docker-compose.override.yml`.
//! [`OVERRIDE_FILENAMES`] exposes that list so a handler can discover the file
//! beside the base WITHOUT this module performing any I/O (it stays pure).
use lightr_core::Result;
use serde_yaml::Value;

use super::super::vars::VarScope;
use super::parse::parse_compose_with_scope;

/// Candidate override filenames, in docker-compose precedence order. The first
/// one that exists beside the base compose file is the one applied. Exposed for
/// handler-side discovery; this module performs no filesystem access.
pub const OVERRIDE_FILENAMES: [&str; 3] = [
    "compose.override.yaml",
    "compose.override.yml",
    "docker-compose.override.yml",
];

/// Deep-merge `override_val` OVER `base` (docker-compose override semantics).
///
/// - Two mappings merge recursively, key by key: a key present only in `base` is
///   kept; a key present in `override_val` replaces/merges into the base value.
/// - Anything that is not a pair of mappings (sequences, scalars, or a type
///   mismatch such as a map overriding a scalar) is REPLACED by `override_val`.
///   In particular sequences are replaced wholesale, never concatenated.
///
/// Pure: no I/O, no global state.
pub fn deep_merge(base: Value, override_val: Value) -> Value {
    match (base, override_val) {
        (Value::Mapping(mut base_map), Value::Mapping(over_map)) => {
            for (k, over_v) in over_map {
                let merged = match base_map.remove(&k) {
                    Some(base_v) => deep_merge(base_v, over_v),
                    None => over_v,
                };
                base_map.insert(k, merged);
            }
            Value::Mapping(base_map)
        }
        // Sequences, scalars, and any base/override type mismatch: override wins.
        (_, override_val) => override_val,
    }
}

/// Parse a base compose document with an OPTIONAL override deep-merged over it,
/// then interpolate + lower via [`parse_compose_with_scope`].
///
/// `override_yaml == None` is byte-identical to calling
/// [`parse_compose_with_scope`] on `base_yaml` (behavior-preserving). When an
/// override is present, both documents are parsed to `serde_yaml::Value`,
/// [`deep_merge`]d (override over base), re-serialized, and the merged text is
/// fed through the normal parse path so `${VAR}` interpolation runs over the
/// merged document exactly as docker-compose does.
///
/// Fail-closed: malformed base/override YAML, a triggered `${VAR:?msg}`, or a
/// structurally-invalid value all surface as honest errors.
pub fn parse_compose_merged(
    base_yaml: &str,
    override_yaml: Option<&str>,
    scope: &VarScope,
) -> Result<super::model::Compose> {
    let override_yaml = match override_yaml {
        // No override → preserve today's path exactly (no re-serialize roundtrip).
        None => return parse_compose_with_scope(base_yaml, scope),
        Some(o) => o,
    };

    // Merge at the Value level BEFORE deserializing into the spec model.
    let merged = merge_yaml(base_yaml, override_yaml)?;
    parse_compose_with_scope(&merged, scope)
}

/// Deep-merge two raw YAML documents and re-serialize the result. An empty
/// document is treated as YAML `null` (its natural deserialization), so an empty
/// override merges to the base and an empty base merges to the override.
fn merge_yaml(base_yaml: &str, override_yaml: &str) -> Result<String> {
    use lightr_core::LightrError;

    let base: Value = serde_yaml::from_str(base_yaml)
        .map_err(|e| LightrError::InvalidManifest(format!("compose parse error: {e}")))?;
    let over: Value = serde_yaml::from_str(override_yaml)
        .map_err(|e| LightrError::InvalidManifest(format!("compose override parse error: {e}")))?;

    let merged = deep_merge(base, over);
    serde_yaml::to_string(&merged)
        .map_err(|e| LightrError::InvalidManifest(format!("compose merge serialize error: {e}")))
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
