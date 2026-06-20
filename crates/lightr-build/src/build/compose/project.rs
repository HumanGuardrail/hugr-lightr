//! CMP-P1-PROJECT: compose project name resolution + sanitization.
//!
//! Docker-compose namespaces a stack with a *project name*: it scopes
//! `compose down`, resource prefixing, and stops two stacks from colliding.
//!
//! Resolution precedence (highest wins), mirroring Docker:
//!   1. the `-p` / `--project-name` CLI flag,
//!   2. the `COMPOSE_PROJECT_NAME` environment variable,
//!   3. the compose file's top-level `name:` field,
//!   4. the sanitized basename of the project directory (the default).
//!
//! Every candidate is run through [`sanitize_project_name`], which lowercases
//! and conforms the value to Docker's project-name grammar
//! `[a-z0-9][a-z0-9_-]*`. Resolution is fail-closed: a candidate that
//! sanitizes to nothing usable (e.g. `"!!!"`) is rejected with an honest error
//! rather than silently substituted (precedence 1–3); the basename fallback
//! (precedence 4) additionally degrades to `"default"` so a pathological cwd
//! never aborts `compose up` — matching Docker, which uses `default` when the
//! working directory yields no usable name.
//!
//! These are pure functions: the environment is read at the CLI call site and
//! the lookup is passed in, keeping the resolver parallel-safe (no
//! process-global env mutation here or in tests).

use lightr_core::{LightrError, Result};

/// The project name Docker falls back to when nothing else yields one.
pub const DEFAULT_PROJECT: &str = "default";

/// Sanitize a candidate into Docker's project-name grammar.
///
/// Docker's rule: the name is lowercased, then restricted to
/// `[a-z0-9][a-z0-9_-]*` — it must start with an ASCII letter or digit, and
/// the remainder may be ASCII letters, digits, `_` or `-`. Any other character
/// is dropped (Docker replaces runs of disallowed characters; we drop them,
/// which is equivalent for the allowed alphabet). Leading characters that are
/// not `[a-z0-9]` are stripped so the result always starts legally.
///
/// Returns `None` when nothing legal survives (e.g. `""`, `"!!!"`, `"___"`).
pub fn sanitize_project_name(raw: &str) -> Option<String> {
    let lowered = raw.to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        let keep = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-';
        if !keep {
            continue;
        }
        // The first kept character must be `[a-z0-9]`; skip leading `_`/`-`.
        if out.is_empty() && !(ch.is_ascii_lowercase() || ch.is_ascii_digit()) {
            continue;
        }
        out.push(ch);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Resolve the effective project name from the four precedence sources.
///
/// * `cli` — the `-p`/`--project-name` flag value, if given.
/// * `env` — the `COMPOSE_PROJECT_NAME` value, if set (read by the caller).
/// * `file_name` — the compose file's top-level `name:` field, if present.
/// * `project_dir_basename` — the basename of the project directory, used for
///   the default. Empty/unusable falls back to [`DEFAULT_PROJECT`].
///
/// Precedence 1–3 are fail-closed: an *explicitly supplied* name that
/// sanitizes to nothing is an honest error (the user asked for a name we
/// cannot honor). The basename fallback never errors — it degrades to
/// [`DEFAULT_PROJECT`].
pub fn resolve_project_name(
    cli: Option<&str>,
    env: Option<&str>,
    file_name: Option<&str>,
    project_dir_basename: &str,
) -> Result<String> {
    if let Some(raw) = cli {
        return sanitize_or_err(raw, "--project-name");
    }
    if let Some(raw) = env {
        return sanitize_or_err(raw, "COMPOSE_PROJECT_NAME");
    }
    if let Some(raw) = file_name {
        return sanitize_or_err(raw, "compose file `name:`");
    }
    Ok(sanitize_project_name(project_dir_basename).unwrap_or_else(|| DEFAULT_PROJECT.to_string()))
}

/// Sanitize an explicitly-supplied candidate or fail closed with the source
/// named, so the user sees *which* input was rejected and why.
fn sanitize_or_err(raw: &str, source: &str) -> Result<String> {
    sanitize_project_name(raw).ok_or_else(|| {
        LightrError::InvalidManifest(format!(
            "invalid compose project name from {source}: {raw:?} \
             (must contain a char in [a-z0-9] after lowercasing; \
             allowed grammar is [a-z0-9][a-z0-9_-]*)"
        ))
    })
}

/// The basename of a project directory, as a `&str`, for the default source.
/// Returns the empty string when the path has no usable final component (the
/// resolver then degrades to [`DEFAULT_PROJECT`]).
pub fn dir_basename(project_dir: &std::path::Path) -> String {
    project_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
