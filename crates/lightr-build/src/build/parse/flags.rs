//! Flag-token parsing for instructions that take `--key=value` options
//! (FROM `--platform`, COPY/ADD `--from`/`--chown`/`--chmod`, HEALTHCHECK opts).
//!
//! Faithful, minimal: a leading run of `--key=value` tokens is captured into
//! `(key, value)` pairs; everything after the first non-flag token is
//! positional. No interpolation, no quoting beyond whitespace splitting.

/// Split a flag-bearing instruction tail into its leading `--key=value` flags
/// and the remaining positional tokens.
///
/// Only `--key=value` tokens at the front are treated as flags; once a
/// non-flag token is seen, the rest are positional (Docker requires flags to
/// precede positionals for these instructions).
pub(super) fn split_flags(rest: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut flags = Vec::new();
    let mut positional = Vec::new();
    let mut in_flags = true;
    for tok in rest.split_ascii_whitespace() {
        if in_flags {
            if let Some(flag) = tok.strip_prefix("--") {
                if let Some((k, v)) = flag.split_once('=') {
                    flags.push((k.to_string(), v.to_string()));
                    continue;
                }
            }
            in_flags = false;
        }
        positional.push(tok.to_string());
    }
    (flags, positional)
}

/// Extract a single named `--<name>=<value>` flag (if present at the front),
/// returning `(Some(value), remaining_tail)`. Used by FROM's `--platform`.
pub(super) fn take_flag(rest: &str, name: &str) -> (Option<String>, String) {
    let (flags, positional) = split_flags(rest);
    let value = flags
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone());
    // Re-emit any flags that were NOT the requested one, preserving order,
    // ahead of the positionals (faithful to "flags precede positionals").
    let mut tail: Vec<String> = flags
        .iter()
        .filter(|(k, _)| k != name)
        .map(|(k, v)| format!("--{k}={v}"))
        .collect();
    tail.extend(positional);
    (value, tail.join(" "))
}
