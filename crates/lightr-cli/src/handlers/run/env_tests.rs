//! WP-RC-1 — `-e`/`--env-file` → env_explicit resolution.
//!
//! Parallel-safe: the testable core `resolve_env_explicit` takes the lead env
//! as an INJECTED closure (never `std::env`), so no test mutates process-global
//! state and `--test-threads=1` is never required.

use super::resolve_env_explicit;
use std::collections::HashMap;

/// A lead-env closure backed by an in-memory map (no `std::env` touch). The
/// closure OWNS its map, so it borrows nothing from `pairs`.
fn lead(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k: &str| map.get(k).cloned()
}

/// An empty lead env (nothing to inherit).
fn empty_lead() -> impl Fn(&str) -> Option<String> {
    |_: &str| None
}

#[test]
fn e_key_val_basic() {
    let out = resolve_env_explicit(&["FOO=bar".to_string()], None, &empty_lead()).unwrap();
    assert_eq!(out, vec![("FOO".to_string(), "bar".to_string())]);
}

#[test]
fn e_repeatable_preserves_order() {
    let out = resolve_env_explicit(
        &["A=1".to_string(), "B=2".to_string(), "C=3".to_string()],
        None,
        &empty_lead(),
    )
    .unwrap();
    assert_eq!(
        out,
        vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
            ("C".to_string(), "3".to_string()),
        ]
    );
}

#[test]
fn e_later_overrides_earlier() {
    let out = resolve_env_explicit(
        &["X=first".to_string(), "X=second".to_string()],
        None,
        &empty_lead(),
    )
    .unwrap();
    assert_eq!(out, vec![("X".to_string(), "second".to_string())]);
}

#[test]
fn e_key_only_inherits_from_lead() {
    let out =
        resolve_env_explicit(&["HOME".to_string()], None, &lead(&[("HOME", "/u/me")])).unwrap();
    assert_eq!(out, vec![("HOME".to_string(), "/u/me".to_string())]);
}

#[test]
fn e_key_only_unset_is_dropped_not_empty() {
    // Docker drops an inherited-but-unset var rather than setting it empty.
    let out = resolve_env_explicit(&["MISSING".to_string()], None, &empty_lead()).unwrap();
    assert!(
        out.is_empty(),
        "unset inherited var must be omitted, got {out:?}"
    );
}

#[test]
fn env_file_kv_comments_and_inherit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("env");
    std::fs::write(
        &path,
        "# a comment\n\nFOO=bar\n  BAZ=qux  \n# another\nHOME\n",
    )
    .unwrap();
    let out = resolve_env_explicit(
        &[],
        Some(path.to_str().unwrap()),
        &lead(&[("HOME", "/u/me")]),
    )
    .unwrap();
    // Note: `  BAZ=qux  ` line is trimmed to `BAZ=qux` (whole-line trim);
    // value `qux` has no surrounding spaces left.
    assert_eq!(
        out,
        vec![
            ("FOO".to_string(), "bar".to_string()),
            ("BAZ".to_string(), "qux".to_string()),
            ("HOME".to_string(), "/u/me".to_string()),
        ]
    );
}

#[test]
fn precedence_e_overrides_env_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("env");
    std::fs::write(&path, "FOO=from_file\nKEEP=file\n").unwrap();
    let out = resolve_env_explicit(
        &["FOO=from_flag".to_string()],
        Some(path.to_str().unwrap()),
        &empty_lead(),
    )
    .unwrap();
    // env-file applied first (FOO=from_file, KEEP=file), then -e overrides FOO.
    // FOO keeps its first-seen position (from the file).
    assert_eq!(
        out,
        vec![
            ("FOO".to_string(), "from_flag".to_string()),
            ("KEEP".to_string(), "file".to_string()),
        ]
    );
}

#[test]
fn empty_inputs_yield_empty() {
    let out = resolve_env_explicit(&[], None, &empty_lead()).unwrap();
    assert!(out.is_empty());
}

#[test]
fn missing_env_file_fails_closed() {
    let err = resolve_env_explicit(&[], Some("/no/such/env/file/xyz"), &empty_lead());
    assert_eq!(
        err,
        Err(2),
        "a missing --env-file must fail closed with exit 2"
    );
}

#[test]
fn empty_key_assignment_rejected() {
    let err = resolve_env_explicit(&["=val".to_string()], None, &empty_lead());
    assert_eq!(err, Err(2), "an empty key must fail closed");
}
