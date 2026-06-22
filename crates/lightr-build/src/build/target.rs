//! WP-C: `docker build --target <stage>` validation, split from `exec.rs` to
//! keep that file under the 400-line godfile cap (behavior-preserving). Declared
//! as a `#[path]` submodule of `exec` and used as `target::validate_target`.
use lightr_core::{LightrError, Result};

use crate::build::parse::{BuildStep, Instr};

/// Validate a `--target <stage>` request against the parsed Dockerfile, BEFORE
/// any work runs (fail closed). Returns the lowercased target (the form the
/// build loop compares stage names against, Docker matches case-insensitively),
/// or `None` when no target was requested. An unknown target is an honest error
/// that lists the known named stages.
pub(super) fn validate_target(target: Option<&str>, steps: &[BuildStep]) -> Result<Option<String>> {
    let target_lc = match target {
        Some(t) => t.to_ascii_lowercase(),
        None => return Ok(None),
    };
    let known: Vec<&str> = steps
        .iter()
        .filter_map(|s| match &s.instr {
            Instr::From { stage: Some(n), .. } => Some(n.as_str()),
            _ => None,
        })
        .collect();
    if known.iter().any(|n| n.eq_ignore_ascii_case(&target_lc)) {
        Ok(Some(target_lc))
    } else {
        Err(LightrError::InvalidManifest(format!(
            "build --target {:?}: no such stage (known named stages: {:?})",
            target.unwrap_or_default(),
            known
        )))
    }
}
