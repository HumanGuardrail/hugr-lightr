//! ARG instruction handling (WP-DF-08): `ARG name[=default]` + `--build-arg`.
//!
//! Docker ARG scoping, transcribed:
//!   - An `ARG` declared BEFORE the first `FROM` is a **global** arg: usable in
//!     `FROM` lines and NOT carried into the build stage unless re-declared
//!     (a bare `ARG name` after `FROM` re-imports the global value).
//!   - An `ARG` after `FROM` is **build-stage-scoped**.
//!   - Value precedence: `--build-arg` override > the line's default > (for a
//!     bare post-`FROM` re-declaration) the inherited global value > unset.
//!     An UNSET arg is not bound, so `${name}` expands to empty.
//!
//! MEMO (R-KEY / WP-DF-BUILDKEY): ARG does NOTHING to the build key. The key
//! hashes the POST-interpolation instruction text, so an ARG USED in an
//! instruction changes that instruction's text → key differs → correct; an
//! UNUSED ARG changes no text → no key change → no rebuild (matches Docker:
//! an unused ARG does not bust the cache).

use super::parse::Instr;
use std::collections::BTreeMap;

/// `--build-arg name=value` overrides (last wins on duplicate). Applied to a
/// declared ARG only — an override with no `ARG` line is ignored (Docker).
pub(crate) type ArgOverrides = BTreeMap<String, String>;

/// Build the override map from the parsed `(name, value)` pairs.
pub(crate) fn overrides_from_pairs(build_args: &[(String, String)]) -> ArgOverrides {
    build_args
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The ARG scope state threaded across the build loop: the FROM boundary and
/// the global-arg values declared before it.
#[derive(Default)]
pub(crate) struct ArgState {
    seen_from: bool,
    global: BTreeMap<String, String>,
}

impl ArgState {
    /// Record that the first `FROM` was reached: global ARGs do NOT cross into
    /// the build stage, so the caller clears the per-stage ARG set; a re-declared
    /// `ARG name` after `FROM` re-imports the global value via [`Self::apply`].
    pub(crate) fn enter_stage(&mut self) {
        self.seen_from = true;
    }

    /// Update the ARG scope for `step` — runs on BOTH the execute and the
    /// cache-hit path (ARG/FROM scope state is NOT persisted in the layer meta,
    /// so it must be re-derived on every step, hit or miss; an ARG line keys
    /// identically across `--build-arg` values, so its step is always a cache
    /// hit yet its binding must still land in the scope). FROM = stage boundary
    /// (the caller clears `stage_args`); ARG = resolve + bind.
    pub(crate) fn sync(
        &mut self,
        step_instr: &Instr,
        overrides: &ArgOverrides,
        stage_args: &mut BTreeMap<String, String>,
    ) {
        match step_instr {
            Instr::From { .. } => {
                self.enter_stage();
                stage_args.clear();
            }
            Instr::Arg { name, default } => {
                self.apply(name, default.as_deref(), overrides, stage_args);
            }
            _ => {}
        }
    }

    /// Resolve one `ARG name[=default]` and update the active ARG scope
    /// (`stage_args`) per Docker precedence + scoping.
    pub(crate) fn apply(
        &mut self,
        name: &str,
        default: Option<&str>,
        overrides: &ArgOverrides,
        stage_args: &mut BTreeMap<String, String>,
    ) {
        let value = overrides
            .get(name)
            .map(String::as_str)
            .or(default)
            .or_else(|| {
                if self.seen_from {
                    self.global.get(name).map(String::as_str)
                } else {
                    None
                }
            });

        match value {
            Some(v) => {
                stage_args.insert(name.to_string(), v.to_string());
                // Pre-FROM declarations are "global": remember the value so a
                // bare post-FROM re-declaration can re-import it.
                if !self.seen_from {
                    self.global.insert(name.to_string(), v.to_string());
                }
            }
            // Unset: clear any stale binding (e.g. re-declared with no value
            // after the stage boundary cleared the per-stage set).
            None => {
                stage_args.remove(name);
            }
        }
    }
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
