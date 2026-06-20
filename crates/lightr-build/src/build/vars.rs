//! Variable interpolation SIGNATURE — frozen by the FREEZE-GATE
//! (parity-contract.md §0 R-VARENG). The full engine (bash modifiers
//! `${A:-d}`/`${A:?msg}`/`${A:+v}`, `\$` literal escape, ENV-over-ARG
//! precedence) is WP-DF-02's job.
//!
//! This freezes the signature + a minimal correct passthrough: a string with no
//! `${}` references returns UNCHANGED. WP-DF-02 fills the modifiers.
//!
//! LEAD DECISION (parity-contract.md §0): compose CONSUMES this fn directly
//! (no fork). If the crate-dep direction ever forbids that, lift `vars` to
//! `lightr-core`; the SIGNATURE is frozen regardless.

use lightr_core::Result;
use std::collections::BTreeMap;

/// The interpolation scope: build ARGs + ENV. ENV takes precedence over ARG
/// (WP-DF-02 enforces the precedence; here it only holds the maps).
#[derive(Clone, Debug, Default)]
pub struct VarScope {
    pub args: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
}

/// Interpolate `${VAR}` / `$VAR` references in `s` against `scope`.
///
/// FREEZE-GATE BEHAVIOUR: minimal correct passthrough — input WITHOUT any `$`
/// is returned unchanged. The full bash-modifier grammar (`${A:-d}`,
/// `${A:?msg}`, `${A:+v}`), `\$` literal escape, and ENV-over-ARG precedence are
/// filled by WP-DF-02. `escape` selects whether `\$` is honoured as a literal
/// `$` (WP-DF-02 wires it; ignored by the passthrough).
pub fn interpolate(s: &str, _scope: &VarScope, _escape: bool) -> Result<String> {
    // WP-DF-02 fills the modifiers. A string with no `$` cannot contain a
    // reference, so the correct result is the input verbatim — true today and
    // a stable base case once the engine lands.
    if !s.contains('$') {
        return Ok(s.to_string());
    }
    // A `$` is present: the real grammar is WP-DF-02's. Until it lands, the
    // minimal correct (lossless) behaviour is to leave the text untouched
    // rather than half-interpret it. WP-DF-02 replaces this branch.
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_without_refs_is_identity() {
        let scope = VarScope::default();
        assert_eq!(
            interpolate("plain text", &scope, false).unwrap(),
            "plain text"
        );
        assert_eq!(interpolate("", &scope, true).unwrap(), "");
    }
}
