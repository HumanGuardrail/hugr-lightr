//! Tests for the `env_file` line loader (CMP-P0-ENVFILE-SVC).
//!
//! Parallel-safe: each filesystem test uses its own `tempfile::TempDir`; the
//! process-env passthrough is injected as a closure (no global state touched).
use std::collections::BTreeMap;

use super::*;

/// A fixed-map env lookup for the bare-key passthrough (no process-global read).
fn fixed(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k: &str| map.get(k).cloned()
}

#[test]
fn key_val_comments_and_blanks() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("a.env");
    std::fs::write(
        &path,
        "# a comment\n\nFOO=bar\n  # indented comment\nBAZ=qux=with=eq\n\n",
    )
    .unwrap();

    let pairs = read_env_file(&path, &fixed(&[])).unwrap();
    assert_eq!(
        pairs,
        vec![
            ("FOO".to_string(), "bar".to_string()),
            ("BAZ".to_string(), "qux=with=eq".to_string()),
        ]
    );
}

#[test]
fn export_prefix_is_stripped_and_key_trimmed() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("b.env");
    std::fs::write(&path, "export FOO=bar\n  SPACED  =val\n").unwrap();

    let pairs = read_env_file(&path, &fixed(&[])).unwrap();
    assert_eq!(
        pairs,
        vec![
            ("FOO".to_string(), "bar".to_string()),
            ("SPACED".to_string(), "val".to_string()),
        ]
    );
}

#[test]
fn bare_key_passthrough_present_and_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("c.env");
    std::fs::write(&path, "PRESENT\nABSENT\nINLINE=x\n").unwrap();

    let pairs = read_env_file(&path, &fixed(&[("PRESENT", "from-env")])).unwrap();
    // PRESENT passes through from the (injected) process env; ABSENT is dropped.
    assert_eq!(
        pairs,
        vec![
            ("PRESENT".to_string(), "from-env".to_string()),
            ("INLINE".to_string(), "x".to_string()),
        ]
    );
}

#[test]
fn missing_file_is_honest_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("does-not-exist.env");
    let err = read_env_file(&path, &fixed(&[])).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("env_file not found"), "got: {msg}");
}
