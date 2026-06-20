//! Variable interpolation ENGINE — Docker/bash-faithful `${VAR}` substitution.
//! Signature frozen by the FREEZE-GATE (parity-contract.md §0 R-VARENG);
//! WP-DF-02 fills the engine. PURE + deterministic: same `(s, scope, escape)`
//! always yields the same result, no I/O, no process-global state — so it is
//! parallel-safe by construction.
//!
//! Grammar (Docker reference + POSIX parameter expansion):
//!   - `$VAR`        — bare ref; name = `[A-Za-z_][A-Za-z0-9_]*`.
//!   - `${VAR}`      — braced ref.
//!   - `${VAR:-d}`   — `d` if VAR unset OR empty.
//!   - `${VAR-d}`    — `d` if VAR unset only (empty allowed).
//!   - `${VAR:+a}`   — `a` if VAR set AND non-empty.
//!   - `${VAR+a}`    — `a` if VAR set (empty allowed).
//!   - `${VAR:?e}`   — error `e` if VAR unset OR empty (fail-closed).
//!   - `${VAR?e}`    — error `e` if VAR unset only (fail-closed).
//!   - `\$`          — literal `$` when `escape` is true.
//!   - unset ref, no modifier → empty string (Docker behaviour, NOT an error).
//!   - unclosed `${...` → fail-closed error.
//!
//! LEAD DECISION (parity-contract.md §0): compose CONSUMES this fn directly.
//! The SIGNATURE is frozen regardless of crate-dep direction.

use lightr_core::{LightrError, Result};
use std::collections::BTreeMap;

/// The interpolation scope: build ARGs + ENV. ENV takes precedence over ARG.
#[derive(Clone, Debug, Default)]
pub struct VarScope {
    pub args: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
}

impl VarScope {
    /// Resolve a variable. ENV wins over ARG (Docker/compose precedence).
    fn get(&self, name: &str) -> Option<&str> {
        self.env
            .get(name)
            .or_else(|| self.args.get(name))
            .map(String::as_str)
    }
}

/// Interpolate `${VAR}` / `$VAR` references in `s` against `scope`.
///
/// ENV-over-ARG precedence, bash modifiers (`:-`/`-`/`:+`/`+`/`:?`/`?`), and
/// `\$` literal escape (gated on `escape`, the Dockerfile escape directive).
/// Unset refs with no modifier expand to empty (Docker). Unclosed `${` and a
/// triggered `:?`/`?` are fail-closed errors.
pub fn interpolate(s: &str, scope: &VarScope, escape: bool) -> Result<String> {
    // No `$` cannot contain a reference — input is verbatim (stable base case).
    if !s.contains('$') {
        return Ok(s.to_string());
    }

    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\\' if escape => {
                // `\$` → literal `$`; any other `\x` is preserved verbatim
                // (Docker only special-cases the escape char before `$`).
                match chars.peek() {
                    Some('$') => {
                        out.push('$');
                        chars.next();
                    }
                    _ => out.push('\\'),
                }
            }
            '$' => match chars.peek() {
                Some('{') => {
                    chars.next(); // consume '{'
                    out.push_str(&expand_braced(&mut chars, scope)?);
                }
                Some(&n) if is_name_start(n) => {
                    let name = read_bare_name(&mut chars);
                    out.push_str(scope.get(&name).unwrap_or(""));
                }
                // `$` not starting a valid ref (e.g. `$1`, `$ `, trailing `$`)
                // is a literal dollar (Docker leaves it untouched).
                _ => out.push('$'),
            },
            other => out.push(other),
        }
    }

    Ok(out)
}

/// Read a bare `$VAR` name (first char already confirmed a name-start).
fn read_bare_name(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if is_name_char(c) {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    name
}

/// Expand a `${...}` reference; the opening `{` is already consumed.
fn expand_braced(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    scope: &VarScope,
) -> Result<String> {
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if is_name_char(c) {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }

    match chars.next() {
        Some('}') => Ok(scope.get(&name).unwrap_or("").to_string()),
        Some(op) => {
            // A leading ':' makes the test empty-sensitive (`:-` vs `-`).
            let colon = op == ':';
            let operator = if colon {
                chars.next().ok_or_else(|| unclosed(&name))?
            } else {
                op
            };
            let word = read_word(chars, &name)?;
            apply_modifier(&name, colon, operator, &word, scope)
        }
        None => Err(unclosed(&name)),
    }
}

/// Read the modifier word up to the closing `}` (already past the operator).
fn read_word(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, name: &str) -> Result<String> {
    let mut word = String::new();
    for c in chars.by_ref() {
        if c == '}' {
            return Ok(word);
        }
        word.push(c);
    }
    Err(unclosed(name))
}

/// Apply a POSIX/bash modifier. `colon` = empty counts as unset.
fn apply_modifier(
    name: &str,
    colon: bool,
    operator: char,
    word: &str,
    scope: &VarScope,
) -> Result<String> {
    let value = scope.get(name);
    // "Use default / triggers" when unset, or (colon) when set-but-empty.
    let unset_or_empty = match value {
        None => true,
        Some(v) => colon && v.is_empty(),
    };

    match operator {
        '-' => Ok(if unset_or_empty {
            word.to_string()
        } else {
            value.unwrap_or("").to_string()
        }),
        '+' => Ok(if unset_or_empty {
            String::new()
        } else {
            word.to_string()
        }),
        '?' => {
            if unset_or_empty {
                let detail = if !word.is_empty() {
                    word.to_string()
                } else if colon {
                    "parameter null or not set".to_string()
                } else {
                    "parameter not set".to_string()
                };
                Err(LightrError::InvalidManifest(format!(
                    "${{{name}}}: {detail}"
                )))
            } else {
                Ok(value.unwrap_or("").to_string())
            }
        }
        // Unknown operator (e.g. `${VAR=d}`, `${VAR#p}`) is not part of the
        // Docker interpolation surface — fail closed rather than mis-expand.
        other => Err(LightrError::InvalidManifest(format!(
            "${{{name}}}: unsupported operator '{other}'"
        ))),
    }
}

fn unclosed(name: &str) -> LightrError {
    LightrError::InvalidManifest(format!("unclosed variable reference: ${{{name}"))
}

fn is_name_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_name_char(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(args: &[(&str, &str)], envs: &[(&str, &str)]) -> VarScope {
        VarScope {
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            env: envs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn passthrough_without_refs_is_identity() {
        let s = VarScope::default();
        assert_eq!(interpolate("plain text", &s, false).unwrap(), "plain text");
        assert_eq!(interpolate("", &s, true).unwrap(), "");
    }

    #[test]
    fn bare_and_braced_substitution() {
        let s = scope(&[("A", "1")], &[("B", "2")]);
        assert_eq!(interpolate("$A-$B", &s, false).unwrap(), "1-2");
        assert_eq!(interpolate("${A}-${B}", &s, false).unwrap(), "1-2");
        assert_eq!(interpolate("x${A}y", &s, false).unwrap(), "x1y");
    }

    #[test]
    fn env_wins_over_arg() {
        let s = scope(&[("V", "from_arg")], &[("V", "from_env")]);
        assert_eq!(interpolate("$V", &s, false).unwrap(), "from_env");
        assert_eq!(interpolate("${V}", &s, false).unwrap(), "from_env");
    }

    #[test]
    fn unset_no_modifier_is_empty() {
        let s = VarScope::default();
        assert_eq!(interpolate("[$X]", &s, false).unwrap(), "[]");
        assert_eq!(interpolate("[${X}]", &s, false).unwrap(), "[]");
    }

    #[test]
    fn default_colon_dash_uses_default_when_unset_or_empty() {
        let s = scope(&[("E", "")], &[]);
        assert_eq!(interpolate("${X:-d}", &s, false).unwrap(), "d"); // unset
        assert_eq!(interpolate("${E:-d}", &s, false).unwrap(), "d"); // empty
        let set = scope(&[("X", "v")], &[]);
        assert_eq!(interpolate("${X:-d}", &set, false).unwrap(), "v");
    }

    #[test]
    fn default_dash_uses_default_only_when_unset() {
        let s = scope(&[("E", "")], &[]);
        assert_eq!(interpolate("${X-d}", &s, false).unwrap(), "d"); // unset
        assert_eq!(interpolate("${E-d}", &s, false).unwrap(), ""); // empty kept
    }

    #[test]
    fn alt_colon_plus_uses_alt_when_set_nonempty() {
        let set = scope(&[("X", "v")], &[]);
        assert_eq!(interpolate("${X:+a}", &set, false).unwrap(), "a");
        let empty = scope(&[("E", "")], &[]);
        assert_eq!(interpolate("${E:+a}", &empty, false).unwrap(), ""); // empty
        let unset = VarScope::default();
        assert_eq!(interpolate("${X:+a}", &unset, false).unwrap(), "");
    }

    #[test]
    fn alt_plus_uses_alt_when_set_even_if_empty() {
        let empty = scope(&[("E", "")], &[]);
        assert_eq!(interpolate("${E+a}", &empty, false).unwrap(), "a");
        let unset = VarScope::default();
        assert_eq!(interpolate("${X+a}", &unset, false).unwrap(), "");
    }

    #[test]
    fn error_colon_question_when_unset_or_empty() {
        let s = scope(&[("E", "")], &[]);
        let e = interpolate("${X:?must set X}", &s, false).unwrap_err();
        assert!(e.to_string().contains("must set X"), "{e}");
        assert!(interpolate("${E:?nope}", &s, false).is_err()); // empty triggers
        let set = scope(&[("X", "v")], &[]);
        assert_eq!(interpolate("${X:?nope}", &set, false).unwrap(), "v");
    }

    #[test]
    fn error_question_only_when_unset() {
        let empty = scope(&[("E", "")], &[]);
        assert_eq!(interpolate("${E?nope}", &empty, false).unwrap(), ""); // empty ok
        let unset = VarScope::default();
        assert!(interpolate("${X?gone}", &unset, false).is_err());
    }

    #[test]
    fn question_empty_word_uses_canonical_message() {
        let unset = VarScope::default();
        let e = interpolate("${X:?}", &unset, false).unwrap_err();
        assert!(e.to_string().contains("null or not set"), "{e}");
    }

    #[test]
    fn escape_on_makes_literal_dollar() {
        let s = scope(&[("A", "1")], &[]);
        assert_eq!(interpolate("\\$A", &s, true).unwrap(), "$A");
        assert_eq!(interpolate("\\${A}", &s, true).unwrap(), "${A}");
    }

    #[test]
    fn escape_off_backslash_is_literal_and_var_expands() {
        let s = scope(&[("A", "1")], &[]);
        // escape disabled: backslash kept, `$A` still expands.
        assert_eq!(interpolate("\\$A", &s, false).unwrap(), "\\1");
    }

    #[test]
    fn unclosed_brace_is_error() {
        let s = VarScope::default();
        assert!(interpolate("${A", &s, false).is_err());
        assert!(interpolate("${A:-d", &s, false).is_err());
        assert!(interpolate("${A:", &s, false).is_err());
    }

    #[test]
    fn lone_dollar_is_literal() {
        let s = VarScope::default();
        assert_eq!(interpolate("cost is $5", &s, false).unwrap(), "cost is $5");
        assert_eq!(interpolate("trailing$", &s, false).unwrap(), "trailing$");
        assert_eq!(interpolate("$ space", &s, false).unwrap(), "$ space");
    }

    #[test]
    fn unsupported_operator_fails_closed() {
        let s = scope(&[("A", "1")], &[]);
        assert!(interpolate("${A=d}", &s, false).is_err());
        assert!(interpolate("${A#p}", &s, false).is_err());
    }

    #[test]
    fn name_stops_at_non_name_char_in_bare_form() {
        let s = scope(&[("A", "X")], &[]);
        assert_eq!(interpolate("$A.txt", &s, false).unwrap(), "X.txt");
        assert_eq!(interpolate("$A-$A", &s, false).unwrap(), "X-X");
    }
}
