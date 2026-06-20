//! Tests for compose `${VAR}` interpolation + `.env` loading.
//!
//! PARALLEL-SAFE: every test injects its `VarScope` directly (or builds it from
//! a per-test tempdir). No test mutates `std::env` or the cwd, so the suite runs
//! clean MULTI-THREADED (no `--test-threads=1`).

use super::super::super::vars::VarScope;
use super::super::parse::parse_compose_with_scope;
use super::{interpolate_compose, parse_dotenv};
use std::collections::BTreeMap;

/// Build a `VarScope` with only the env map populated (compose has no ARGs).
fn env_scope(pairs: &[(&str, &str)]) -> VarScope {
    VarScope {
        args: BTreeMap::new(),
        env: pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

// ── interpolate_compose: substitution forms ───────────────────────────────────

#[test]
fn braced_and_bare_substitution() {
    let s = env_scope(&[("TAG", "1.2"), ("PORT", "8080")]);
    let yaml = "image: nginx:${TAG}\nport: $PORT";
    assert_eq!(
        interpolate_compose(yaml, &s).unwrap(),
        "image: nginx:1.2\nport: 8080"
    );
}

#[test]
fn default_modifier_colon_dash() {
    let s = env_scope(&[]);
    assert_eq!(
        interpolate_compose("t: ${TAG:-latest}", &s).unwrap(),
        "t: latest"
    );
    let set = env_scope(&[("TAG", "edge")]);
    assert_eq!(
        interpolate_compose("t: ${TAG:-latest}", &set).unwrap(),
        "t: edge"
    );
}

#[test]
fn required_modifier_colon_question_unset_is_error() {
    let s = env_scope(&[]);
    let e = interpolate_compose("t: ${TAG:?TAG is required}", &s).unwrap_err();
    assert!(e.to_string().contains("TAG is required"), "{e}");
}

#[test]
fn required_modifier_colon_question_set_passes() {
    let s = env_scope(&[("TAG", "v1")]);
    assert_eq!(interpolate_compose("t: ${TAG:?nope}", &s).unwrap(), "t: v1");
}

// ── interpolate_compose: $$ literal escape (compose rule, NOT backslash) ───────

#[test]
fn double_dollar_is_literal_single_dollar() {
    let s = env_scope(&[("VAR", "expanded")]);
    // `$$VAR` must NOT expand — `$$` collapses to a literal `$`, leaving `$VAR`
    // as literal text in the document.
    assert_eq!(
        interpolate_compose("cmd: echo $$VAR and ${VAR}", &s).unwrap(),
        "cmd: echo $VAR and expanded"
    );
}

#[test]
fn double_dollar_pair_collapses() {
    let s = env_scope(&[]);
    assert_eq!(
        interpolate_compose("price: $$5.00", &s).unwrap(),
        "price: $5.00"
    );
    // Two adjacent `$$$$` → `$$`.
    assert_eq!(interpolate_compose("$$$$", &s).unwrap(), "$$");
}

#[test]
fn backslash_is_not_an_escape_in_compose() {
    // Compose has no `\$` rule: the backslash stays literal AND `$A` expands.
    let s = env_scope(&[("A", "X")]);
    assert_eq!(interpolate_compose("\\$A", &s).unwrap(), "\\X");
}

// ── behavior-preservation: no refs → identity, parses like parse_compose ───────

#[test]
fn no_refs_is_identity() {
    let s = env_scope(&[("UNUSED", "v")]);
    let yaml = "services:\n  web:\n    image: nginx:latest\n";
    assert_eq!(interpolate_compose(yaml, &s).unwrap(), yaml);
}

#[test]
fn parse_with_scope_matches_plain_parse_when_no_refs() {
    let yaml = "services:\n  web:\n    image: nginx:1.25\n  db:\n    image: postgres:16\n";
    let plain = super::super::parse::parse_compose(yaml).unwrap();
    let interp = parse_compose_with_scope(yaml, &env_scope(&[])).unwrap();
    assert_eq!(plain.services.len(), interp.services.len());
    for (a, b) in plain.services.iter().zip(interp.services.iter()) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.image_ref, b.image_ref);
    }
}

#[test]
fn parse_with_scope_substitutes_image_tag() {
    let yaml = "services:\n  web:\n    image: nginx:${TAG}\n";
    let c = parse_compose_with_scope(yaml, &env_scope(&[("TAG", "1.27")])).unwrap();
    assert_eq!(c.services.len(), 1);
    assert_eq!(c.services[0].image_ref, "nginx:1.27");
}

#[test]
fn parse_with_scope_missing_required_errors() {
    let yaml = "services:\n  web:\n    image: nginx:${TAG:?set TAG}\n";
    // `Compose` is not `Debug`, so we can't `.unwrap_err()` — match instead.
    match parse_compose_with_scope(yaml, &env_scope(&[])) {
        Ok(_) => panic!("expected missing-required error"),
        Err(e) => assert!(e.to_string().contains("set TAG"), "{e}"),
    }
}

// ── .env parsing + precedence (process env wins) ───────────────────────────────

#[test]
fn parse_dotenv_basic_lines() {
    let text = "# comment\nFOO=bar\n\nexport BAZ=qux\nKEY = spaced\n";
    let pairs = parse_dotenv(text);
    assert_eq!(
        pairs,
        vec![
            ("FOO".to_string(), "bar".to_string()),
            ("BAZ".to_string(), "qux".to_string()),
            ("KEY".to_string(), "spaced".to_string()),
        ]
    );
}

#[test]
fn parse_dotenv_strips_matching_quotes() {
    let pairs = parse_dotenv("A=\"double\"\nB='single'\nC=\"mismatch'\n");
    assert_eq!(pairs[0], ("A".to_string(), "double".to_string()));
    assert_eq!(pairs[1], ("B".to_string(), "single".to_string()));
    // Mismatched quotes are left untouched.
    assert_eq!(pairs[2], ("C".to_string(), "\"mismatch'".to_string()));
}

#[test]
fn parse_dotenv_skips_malformed_lines() {
    let pairs = parse_dotenv("no_equals_here\n=novalue\nGOOD=1\n");
    assert_eq!(pairs, vec![("GOOD".to_string(), "1".to_string())]);
}

#[test]
fn scope_from_project_dir_loads_dotenv_lower_precedence() {
    // We can't mutate the process env safely under parallel tests, so this test
    // verifies the `.env` path only and asserts process-env precedence with a
    // value the process env is overwhelmingly unlikely to define.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join(".env"), "CMP_INTERP_TEST_KEY=from_dotenv\n").unwrap();
    let scope = super::scope_from_project_dir(dir.path());
    assert_eq!(
        scope.env.get("CMP_INTERP_TEST_KEY").map(String::as_str),
        Some("from_dotenv")
    );
}

#[test]
fn scope_from_project_dir_process_env_wins_over_dotenv() {
    // PATH is virtually always set in the process env; a `.env` PATH must lose.
    let real_path = std::env::var("PATH").unwrap_or_default();
    if real_path.is_empty() {
        return; // no process PATH to assert precedence against — skip.
    }
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join(".env"), "PATH=dotenv_should_lose\n").unwrap();
    let scope = super::scope_from_project_dir(dir.path());
    assert_eq!(
        scope.env.get("PATH").map(String::as_str),
        Some(&real_path[..])
    );
}

#[test]
fn scope_from_project_dir_missing_dotenv_is_ok() {
    let dir = tempfile::TempDir::new().unwrap();
    // No `.env` written — must not panic, just yields process env.
    let scope = super::scope_from_project_dir(dir.path());
    assert!(scope.args.is_empty());
}
