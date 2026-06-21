//! Multi-stage build stage table (WP-DF-03), split from `exec.rs` to keep both
//! files under the 400-line godfile cap (behavior-preserving — byte-identical
//! logic to the prior single `exec.rs`). Declared as a `#[path]` submodule of
//! `exec` and re-exported there (`pub(super) use stage::StageTable;`) so every
//! call site — `super::exec::StageTable` in `exec_instr.rs`, the doc-links in
//! `exec_instr_copy.rs`, and `StageTable::default()` in `exec.rs::build()` —
//! stays IDENTICAL.
use lightr_core::{Digest, LightrError, Result};

/// The multi-stage stage table (WP-DF-03): the resolved output of every stage
/// that has already finished building, in build order. A `FROM <base> [AS name]`
/// starts a stage; when it completes, its result tree's manifest [`Digest`] is
/// recorded here under its 0-based index AND (when named) its lowercased name.
/// `COPY --from=<name|index>` resolves against this table — so a stage can only
/// reference a PRIOR stage (forward/self refs are absent ⇒ honest error).
#[derive(Default)]
pub(crate) struct StageTable {
    /// Each finished stage's output digest, in build order (index = position).
    by_index: Vec<Digest>,
    /// `name → output digest` for `FROM ... AS <name>` stages (lowercased;
    /// Docker matches stage names case-insensitively).
    by_name: std::collections::HashMap<String, Digest>,
}

impl StageTable {
    /// Record a finished stage's output digest at its build-order index, and
    /// (if the stage was named `AS <name>`) under its lowercased name.
    pub(crate) fn record(&mut self, name: Option<&str>, digest: Digest) {
        self.by_index.push(digest);
        if let Some(n) = name {
            self.by_name.insert(n.to_ascii_lowercase(), digest);
        }
    }

    /// Resolve a `COPY --from=<ref>` to a PRIOR stage's output digest. `ref` is a
    /// stage NAME (case-insensitive) or a 0-based INDEX (a purely-numeric ref is
    /// an index; otherwise a name). Unknown name, out-of-range / self / forward
    /// index → honest fail-closed error (no silent half-copy). The external-IMAGE
    /// `--from=<image>` form is OUT OF SCOPE for this WP: such a ref is neither a
    /// known stage name nor a valid prior index, so it surfaces the same honest
    /// "unknown stage / external image out of scope" error.
    pub(crate) fn resolve(&self, from: &str) -> Result<Digest> {
        if !from.is_empty() && from.chars().all(|c| c.is_ascii_digit()) {
            let idx: usize = from.parse().map_err(|_| {
                LightrError::InvalidManifest(format!("COPY --from: invalid stage index {from:?}"))
            })?;
            return self.by_index.get(idx).copied().ok_or_else(|| {
                LightrError::InvalidManifest(format!(
                    "COPY --from={from}: no such prior stage (index out of range — \
                     forward/self references are not allowed)"
                ))
            });
        }
        self.by_name
            .get(&from.to_ascii_lowercase())
            .copied()
            .ok_or_else(|| {
                LightrError::InvalidManifest(format!(
                    "COPY --from={from}: unknown stage name (only PRIOR named stages are \
                     valid; copying --from an external image is out of scope)"
                ))
            })
    }
}
