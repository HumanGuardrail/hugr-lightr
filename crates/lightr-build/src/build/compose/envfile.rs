//! Service `env_file` loader (compose-spec `services.<name>.env_file`).
//!
//! A compose service may list one or more env files whose `KEY=VAL` lines
//! become service environment variables, at LOWER precedence than the inline
//! `environment:` block (inline overrides file; later files override earlier).
//! Paths are resolved relative to the compose file's directory.
//!
//! Line grammar (compose `env_file` subset, faithful to docker-compose):
//!   - blank lines and `#` comment lines are ignored;
//!   - `KEY=VAL` sets `KEY` to `VAL` (the first `=` splits; the value keeps any
//!     further `=`); leading `export ` is stripped and the key is trimmed;
//!   - a bare `KEY` line (no `=`) is a PASSTHROUGH: the value is taken from the
//!     process environment, and the entry is DROPPED if the process env has no
//!     such variable (compose's documented behavior).
//!
//! PURITY / parallel-safety: the process-env passthrough is injected as a
//! lookup closure (`env_lookup`), so tests provide a fixed map and never read
//! or mutate process-global state. The production call site passes a closure
//! over `std::env::var`.
//!
//! Unlike `interp::parse_dotenv` (which parses the project `.env` for `${VAR}`
//! interpolation and SKIPS bare keys), this loader honors the env_file-specific
//! bare-key passthrough rule, so it is a distinct parser.

use std::path::Path;

use lightr_core::{LightrError, Result};

/// Read one env file and return its `(KEY, VAL)` entries in declaration order.
///
/// `env_lookup` resolves a bare-key passthrough against the process env (inject
/// `|k| std::env::var(k).ok()` in production; a fixed map in tests). A
/// required-but-missing file is an honest error (compose errors when a listed
/// env_file is absent).
pub(crate) fn read_env_file(
    path: &Path,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        LightrError::InvalidManifest(format!("env_file not found: {}: {e}", path.display()))
    })?;
    Ok(parse_env_file(&text, env_lookup))
}

/// Parse env-file text into ordered `(KEY, VAL)` pairs. Pure (the only outside
/// dependency is the injected `env_lookup`).
fn parse_env_file(
    text: &str,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        match line.split_once('=') {
            Some((key, value)) => {
                let key = key.trim();
                if key.is_empty() {
                    continue;
                }
                pairs.push((key.to_string(), value.to_string()));
            }
            None => {
                // Bare `KEY`: passthrough from the process env; drop if unset.
                let key = line.trim();
                if key.is_empty() {
                    continue;
                }
                if let Some(val) = env_lookup(key) {
                    pairs.push((key.to_string(), val));
                }
            }
        }
    }
    pairs
}

#[cfg(test)]
#[path = "envfile_tests.rs"]
mod tests;
