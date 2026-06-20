//! Compose-spec `${VAR}` interpolation + `.env` scope construction.
//!
//! LEAD DECISION (R-VARENG, parity-contract.md §0): compose REUSES the frozen
//! `vars::interpolate` engine — it does NOT fork a second interpolation engine.
//! Compose interpolates the WHOLE raw YAML document BEFORE serde parsing
//! (compose-spec applies interpolation to the entire file, not per-value), then
//! `parse_compose` deserializes the substituted text.
//!
//! Two compose-spec deviations from the Dockerfile engine are handled here:
//!   1. Compose's escape for a literal `$` is `$$`, NOT `\$`. `vars::interpolate`
//!      only knows the Dockerfile `\$` rule, so we pre/post-process: split the
//!      document on `$$`, interpolate each segment with `escape=false` (a lone
//!      `\` must stay literal in YAML), then rejoin the segments with a literal
//!      `$`. Splitting first guarantees a `$$` is never seen by the engine as a
//!      variable reference, and a `\` is never treated as an escape.
//!   2. Precedence: the process environment WINS over a `.env` file (compose
//!      rule). `.env` is the lower-precedence source.
//!
//! PURITY: `interpolate_compose` is pure — it takes the scope as a parameter, so
//! tests inject env/`.env` without touching process-global state (parallel-safe
//! by construction). Only `scope_from_project_dir` reads the real process env;
//! that helper is for the CLI call site, not the test path.

use std::collections::BTreeMap;
use std::path::Path;

use lightr_core::Result;

use super::super::vars::{interpolate, VarScope};

/// Interpolate compose `${VAR}` references across the whole raw YAML document.
///
/// Honors compose's `$$` → literal `$` escape (handled here, not by the engine)
/// and the engine's `${VAR}` / `${VAR:-d}` / `${VAR:?e}` / `$VAR` grammar with
/// process-env-over-`.env` precedence carried by `scope`. Fail-closed: a
/// triggered `${VAR:?msg}` or an unclosed `${` returns an honest error.
pub fn interpolate_compose(yaml: &str, scope: &VarScope) -> Result<String> {
    // Compose escape: `$$` is a literal `$`. Split on `$$` so the engine never
    // sees a doubled dollar as a reference, then interpolate each segment with
    // the Dockerfile backslash-escape DISABLED (compose has no `\$` rule), and
    // rejoin with a literal `$`.
    let mut out = String::with_capacity(yaml.len());
    let mut segments = yaml.split("$$");
    if let Some(first) = segments.next() {
        out.push_str(&interpolate(first, scope, false)?);
    }
    for seg in segments {
        out.push('$');
        out.push_str(&interpolate(seg, scope, false)?);
    }
    Ok(out)
}

/// Build a `VarScope` for compose interpolation from a project directory.
///
/// Precedence (compose rule): a `.env` file in `project_dir` is the LOWER source
/// and the live process environment WINS. A missing/unreadable `.env` is not an
/// error (compose treats it as absent). The result is suitable for
/// [`interpolate_compose`]. Reads the process env — intended for the CLI call
/// site, NOT the test path (tests build `VarScope` directly to stay
/// parallel-safe).
pub fn scope_from_project_dir(project_dir: &Path) -> VarScope {
    let mut env: BTreeMap<String, String> = BTreeMap::new();

    // Lower-precedence: `.env` file (best-effort; absent is fine).
    if let Ok(text) = std::fs::read_to_string(project_dir.join(".env")) {
        for (k, v) in parse_dotenv(&text) {
            env.insert(k, v);
        }
    }
    // Higher-precedence: process environment overwrites `.env`.
    for (k, v) in std::env::vars() {
        env.insert(k, v);
    }

    VarScope {
        args: BTreeMap::new(),
        env,
    }
}

/// Parse a `.env` file: `KEY=VALUE` lines, `#` comments, blank lines ignored.
///
/// Faithful to the compose `.env` subset: leading `export ` is stripped, the
/// first `=` splits key from value, surrounding whitespace on the key is
/// trimmed, and a single layer of matching quotes around the value is removed.
/// No interpolation is performed on `.env` values here (compose's nested-`.env`
/// expansion is a later WP — noted in the card).
pub fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue; // a line with no `=` is not a KEY=VAL entry — skip it.
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = unquote(value.trim());
        pairs.push((key.to_string(), value));
    }
    pairs
}

/// Strip one matching pair of surrounding single or double quotes.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
#[path = "interp_tests.rs"]
mod tests;
