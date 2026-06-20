//! WP-RC-1 — resolve `-e`/`--env-file` into the keyed `env_explicit` channel.
//!
//! Docker-faithful semantics (transcribed, not designed):
//!
//! - `-e KEY=VAL` sets `KEY` to the literal `VAL`.
//! - `-e KEY` INHERITS `KEY` from the lead (this `lightr` process) env; if `KEY`
//!   is unset in the lead env, the variable is OMITTED (docker drops an unset
//!   inherited var — it is NOT set empty).
//! - `--env-file <path>` reads each non-blank line as `KEY=VAL` or `KEY`
//!   (inherit); `#`-prefixed lines and blank lines are comments/ignored.
//!
//! Precedence (Docker): `--env-file` is applied FIRST, then `-e` overrides it;
//! within either source, a LATER entry overrides an EARLIER one. We model this
//! with an insertion-ordered last-write-wins accumulator so the resolved set is
//! deterministic. The resulting pairs land in `RunSpec.env_explicit` /
//! `SpecOnDisk.env_explicit` — the ONLY env in the run memo key (R-KEY); the
//! key fold sorts them, so CLI order never changes the key but a different
//! KEY/VALUE always does.

/// An insertion-ordered, last-write-wins map of resolved env pairs. Keeps the
/// first-seen position stable while letting a later assignment update the value
/// (Docker precedence) — so the output is deterministic regardless of how many
/// times a key is set.
#[derive(Default)]
struct OrderedEnv {
    /// `(key, value)` in first-insertion order; value overwritten in place.
    pairs: Vec<(String, String)>,
}

impl OrderedEnv {
    fn set(&mut self, key: String, value: String) {
        if let Some(slot) = self.pairs.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.pairs.push((key, value));
        }
    }

    /// Remove a key (docker drops an inherited-but-unset variable rather than
    /// setting it empty). No-op if absent.
    fn remove(&mut self, key: &str) {
        self.pairs.retain(|(k, _)| k != key);
    }

    fn into_pairs(self) -> Vec<(String, String)> {
        self.pairs
    }
}

/// Apply ONE `KEY=VAL` / `KEY` assignment to `acc`, reading the lead env for an
/// inherit (`KEY` with no `=`). `lead_env` is injected (not read from the global
/// process env directly) so tests are parallel-safe — no `std::env` dependency.
fn apply_assignment<F>(acc: &mut OrderedEnv, raw: &str, lead_env: &F) -> Result<(), i32>
where
    F: Fn(&str) -> Option<String>,
{
    match raw.split_once('=') {
        Some((key, val)) => {
            if key.is_empty() {
                eprintln!("lightr: invalid env assignment (empty key): {raw}");
                return Err(2);
            }
            acc.set(key.to_string(), val.to_string());
        }
        None => {
            // `KEY` (no `=`) — inherit from the lead env. Unset ⇒ drop.
            let key = raw;
            if key.is_empty() {
                eprintln!("lightr: invalid env assignment (empty key)");
                return Err(2);
            }
            match lead_env(key) {
                Some(v) => acc.set(key.to_string(), v),
                None => acc.remove(key),
            }
        }
    }
    Ok(())
}

/// Parse one `--env-file` body into assignments, applied in file order. `#`
/// comments and blank lines are skipped; leading/trailing whitespace on a line
/// is trimmed (docker trims surrounding whitespace around the whole line).
fn apply_env_file<F>(acc: &mut OrderedEnv, body: &str, lead_env: &F) -> Result<(), i32>
where
    F: Fn(&str) -> Option<String>,
{
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        apply_assignment(acc, line, lead_env)?;
    }
    Ok(())
}

/// Resolve `--env-file` (applied first) then `-e/--env` (override) into the
/// final `env_explicit` pairs, reading the file from disk and the lead env via
/// the injected closure. Returns `Err(exit_code)` (already printed) on a bad
/// file read or a malformed assignment.
pub(super) fn resolve_env_explicit<F>(
    env_set: &[String],
    env_file: Option<&str>,
    lead_env: &F,
) -> Result<Vec<(String, String)>, i32>
where
    F: Fn(&str) -> Option<String>,
{
    let mut acc = OrderedEnv::default();

    // 1. --env-file FIRST (lowest precedence).
    if let Some(path) = env_file {
        let body = std::fs::read_to_string(path).map_err(|e| {
            eprintln!("lightr: cannot read --env-file {path}: {e}");
            2i32
        })?;
        apply_env_file(&mut acc, &body, lead_env)?;
    }

    // 2. -e / --env override the file (and each other, last-wins).
    for raw in env_set {
        apply_assignment(&mut acc, raw, lead_env)?;
    }

    Ok(acc.into_pairs())
}

/// Convenience wrapper that reads the lead env from the real process env. Kept
/// thin so the testable core (`resolve_env_explicit`) never touches `std::env`.
pub(super) fn resolve_env_explicit_from_process(
    env_set: &[String],
    env_file: Option<&str>,
) -> Result<Vec<(String, String)>, i32> {
    resolve_env_explicit(env_set, env_file, &|k| std::env::var(k).ok())
}

#[cfg(test)]
#[path = "env_tests.rs"]
mod tests;
