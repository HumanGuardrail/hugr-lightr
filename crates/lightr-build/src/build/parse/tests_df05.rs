//! WP-DF-05 parser tests: ENV/LABEL multi-pair + quoting + legacy form.
//! Split out of `tests_instr.rs` to keep each file under the 400-line cap.
//! Pure functions; no global state — parallel-safe.
use super::*;

fn parse(text: &str) -> Vec<BuildStep> {
    parse_dockerfile(text).unwrap()
}

/// Extract ENV pairs from the first parsed step (panics if not ENV).
fn env_pairs(text: &str) -> Vec<(String, String)> {
    match &parse(text)[0].instr {
        Instr::Env { pairs } => pairs.clone(),
        other => panic!("expected Env, got {other:?}"),
    }
}

/// Extract LABEL pairs from the first parsed step (panics if not LABEL).
fn label_pairs(text: &str) -> Vec<(String, String)> {
    match &parse(text)[0].instr {
        Instr::Label { pairs } => pairs.clone(),
        other => panic!("expected Label, got {other:?}"),
    }
}

#[test]
fn env_kv_and_space_forms() {
    // `=` single pair.
    assert_eq!(env_pairs("ENV A=1"), vec![("A".into(), "1".into())]);
    // Legacy `KEY value` — whole rest is the value (spaces kept, single pair).
    assert_eq!(
        env_pairs("ENV KEY value with spaces"),
        vec![("KEY".into(), "value with spaces".into())]
    );
}

#[test]
fn env_multi_pair_sets_all_keys() {
    assert_eq!(
        env_pairs("ENV A=1 B=2 C=3"),
        vec![
            ("A".into(), "1".into()),
            ("B".into(), "2".into()),
            ("C".into(), "3".into()),
        ]
    );
}

#[test]
fn env_quoted_values_with_spaces() {
    // Double + single quotes; quotes stripped, spaces inside preserved.
    assert_eq!(
        env_pairs(r#"ENV A="x y" B='z w'"#),
        vec![("A".into(), "x y".into()), ("B".into(), "z w".into())]
    );
    // Escaped quote inside a double-quoted value.
    assert_eq!(
        env_pairs(r#"ENV MSG="say \"hi\"""#),
        vec![("MSG".into(), r#"say "hi""#.into())]
    );
}

#[test]
fn env_unterminated_quote_is_error() {
    assert!(parse_dockerfile(r#"ENV A="unclosed"#).is_err());
    assert!(parse_dockerfile("ENV A='unclosed").is_err());
}

#[test]
fn env_multi_pair_missing_eq_is_error() {
    // First token has `=`, so the form is multi-pair; a later bare token is bad.
    assert!(parse_dockerfile("ENV A=1 BOGUS").is_err());
}

#[test]
fn label_dotted_key() {
    assert_eq!(
        label_pairs("LABEL org.opencontainers.image.version=1.2.3"),
        vec![("org.opencontainers.image.version".into(), "1.2.3".into())]
    );
}

#[test]
fn label_multi_pair_and_quoting() {
    assert_eq!(
        label_pairs(r#"LABEL a=1 b="two words" c='3'"#),
        vec![
            ("a".into(), "1".into()),
            ("b".into(), "two words".into()),
            ("c".into(), "3".into()),
        ]
    );
}
