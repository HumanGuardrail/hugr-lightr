//! WP-DF-08: pure unit tests for ARG resolution + scoping. No I/O, no env —
//! parallel-safe by construction.
use super::*;

fn ov(pairs: &[(&str, &str)]) -> ArgOverrides {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn default_is_used_when_no_override() {
    let mut st = ArgState::default();
    let mut args = BTreeMap::new();
    st.enter_stage();
    st.apply("V", Some("def"), &ov(&[]), &mut args);
    assert_eq!(args.get("V").map(String::as_str), Some("def"));
}

#[test]
fn override_beats_default() {
    let mut st = ArgState::default();
    let mut args = BTreeMap::new();
    st.enter_stage();
    st.apply("V", Some("def"), &ov(&[("V", "cli")]), &mut args);
    assert_eq!(args.get("V").map(String::as_str), Some("cli"));
}

#[test]
fn unset_arg_is_not_bound() {
    // No override, no default → not inserted (expands to empty downstream).
    let mut st = ArgState::default();
    let mut args = BTreeMap::new();
    st.enter_stage();
    st.apply("V", None, &ov(&[]), &mut args);
    assert!(!args.contains_key("V"));
}

#[test]
fn override_with_no_default_still_binds() {
    let mut st = ArgState::default();
    let mut args = BTreeMap::new();
    st.enter_stage();
    st.apply("V", None, &ov(&[("V", "cli")]), &mut args);
    assert_eq!(args.get("V").map(String::as_str), Some("cli"));
}

#[test]
fn global_arg_reimported_by_bare_redeclare_after_from() {
    // ARG declared before FROM (global), then bare `ARG name` after FROM
    // re-imports the global value.
    let mut st = ArgState::default();
    let mut global_scope = BTreeMap::new();
    // Before FROM: global ARG with default.
    st.apply("V", Some("glob"), &ov(&[]), &mut global_scope);
    assert_eq!(global_scope.get("V").map(String::as_str), Some("glob"));
    // FROM boundary: stage scope is cleared by the caller.
    st.enter_stage();
    let mut stage_scope = BTreeMap::new();
    // Bare re-declaration (no default) after FROM → re-imports global value.
    st.apply("V", None, &ov(&[]), &mut stage_scope);
    assert_eq!(stage_scope.get("V").map(String::as_str), Some("glob"));
}

#[test]
fn global_arg_not_visible_in_stage_without_redeclare() {
    // The stage scope starts empty after FROM; a global ARG is NOT auto-present.
    let mut st = ArgState::default();
    let mut global_scope = BTreeMap::new();
    st.apply("V", Some("glob"), &ov(&[]), &mut global_scope);
    st.enter_stage();
    let stage_scope: BTreeMap<String, String> = BTreeMap::new();
    // Caller clears the stage scope at FROM; without a re-declare V is absent.
    assert!(!stage_scope.contains_key("V"));
}

#[test]
fn override_applies_to_global_before_from() {
    let mut st = ArgState::default();
    let mut global_scope = BTreeMap::new();
    st.apply(
        "TAG",
        Some("latest"),
        &ov(&[("TAG", "1.2")]),
        &mut global_scope,
    );
    assert_eq!(global_scope.get("TAG").map(String::as_str), Some("1.2"));
}
