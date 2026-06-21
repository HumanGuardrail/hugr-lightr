//! Storeless fast-path run-key builder — split out of `memo.rs` to keep each file
//! under the 400-line godfile cap (house convention, via `#[path] mod memo_key;`).
//! `build_key` is the no-store fast path (no mounts AND no secrets/configs); any
//! store-backed spec routes through `memo::assemble_key` instead. The two MUST
//! fold identical bytes — see the matching comments in both.

use lightr_core::{Digest, LightrError, Result};
use lightr_index::{scan, Index};

// `memo_key` is included via `#[path] mod memo_key;` INSIDE `memo.rs`, so its
// `super` is the `memo` module (not `run`): `contribute_env_explicit` is a sibling
// item in `memo`, and `RunSpec` lives one level up in `run::types`.
use super::super::types::RunSpec;
use super::contribute_env_explicit;

/// Keep the old build_key for backward-compat within existing tests. Used only on
/// the fast path (no mounts AND no secrets/configs), so it needs no store;
/// `run_memoized_with`/`predict` route any spec with secrets/configs through
/// `assemble_key` (which resolves refs against the store).
pub(crate) fn build_key(spec: &RunSpec) -> Result<Digest> {
    use std::path::PathBuf;

    let mut hasher = blake3::Hasher::new();

    hasher.update(b"lightr/run/v1\0");

    let inputs: Vec<&PathBuf> = if spec.inputs.is_empty() {
        vec![&spec.cwd]
    } else {
        spec.inputs.iter().collect()
    };

    for input_path in inputs {
        let abs_path = if input_path.is_absolute() {
            input_path.clone()
        } else {
            spec.cwd.join(input_path)
        };
        let canonical = abs_path.canonicalize().map_err(LightrError::Io)?;
        let mut index = Index::load_for(&canonical)?;
        let report = scan(&canonical, &mut index)?;
        let rel_path_bytes = input_path.as_os_str().as_encoded_bytes();
        hasher.update(rel_path_bytes);
        hasher.update(b"\0");
        hasher.update(&report.manifest.digest().0);
    }

    for arg in &spec.command {
        let len = arg.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(arg.as_bytes());
    }

    let mut sorted_keys = spec.env_keys.clone();
    sorted_keys.sort();
    for key in &sorted_keys {
        if let Some(val) = std::env::var_os(key) {
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(val.as_encoded_bytes());
            hasher.update(b"\0");
        } else {
            hasher.update(key.as_bytes());
            hasher.update(b"\x01");
        }
    }

    // WP-RC-1 (R-KEY): explicit env — must match `assemble_key` exactly so the
    // fast path and the store path agree. Empty ⇒ no-op (behavior-preserving).
    contribute_env_explicit(&mut hasher, &spec.env_explicit);

    let triple = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    hasher.update(triple.as_bytes());

    // No mount contribution (this fast path is only taken when mounts are empty).
    // No secrets/configs contribution either: this storeless fast path is only
    // reached when secrets AND configs are empty (a non-empty spec routes through
    // `assemble_key`, which has the store to resolve refs — F-309 §0). An empty
    // contribution is a no-op, so the key is identical to today's for the 16
    // existing (empty-vec) callers.

    Ok(Digest(*hasher.finalize().as_bytes()))
}
