//! Tests for the compose override deep-merge engine (CMP-P0-MERGE).
//!
//! Pure + parallel-safe: every test builds its own value/scope locally and never
//! touches process-global state (no env mutation, no cwd, no shared singletons).
use super::*;
use serde_yaml::Value;

fn yaml(s: &str) -> Value {
    serde_yaml::from_str(s).unwrap()
}

// --- deep_merge: maps recurse, override keys win, base-only keys kept ---------

#[test]
fn deep_merge_nested_map_override() {
    let base = yaml(
        "services:\n  web:\n    image: base:1\n    ports:\n      - \"8080:80\"\n  db:\n    image: pg:15\n",
    );
    let over = yaml("services:\n  web:\n    image: over:2\n");
    let merged = deep_merge(base, over);

    // web.image overridden, web.ports kept (base-only), db kept (base-only).
    let services = merged.get("services").unwrap();
    let web = services.get("web").unwrap();
    assert_eq!(web.get("image").unwrap().as_str(), Some("over:2"));
    assert!(
        web.get("ports").is_some(),
        "base-only nested key must survive"
    );
    assert!(
        services.get("db").is_some(),
        "base-only sibling map must survive"
    );
}

#[test]
fn deep_merge_scalar_override() {
    let base = yaml("a: 1\nb: keep\n");
    let over = yaml("a: 2\n");
    let merged = deep_merge(base, over);
    assert_eq!(merged.get("a").unwrap().as_i64(), Some(2));
    assert_eq!(merged.get("b").unwrap().as_str(), Some("keep"));
}

#[test]
fn deep_merge_list_replace_not_append() {
    // Docker-compose's documented list behavior: REPLACE, never concatenate.
    let base = yaml("ports:\n  - \"8080:80\"\n  - \"9090:90\"\n");
    let over = yaml("ports:\n  - \"3000:3000\"\n");
    let merged = deep_merge(base, over);
    let ports = merged.get("ports").unwrap().as_sequence().unwrap();
    assert_eq!(ports.len(), 1, "sequence must be replaced wholesale");
    assert_eq!(ports[0].as_str(), Some("3000:3000"));
}

#[test]
fn deep_merge_type_mismatch_override_wins() {
    // Override map over base scalar (and vice-versa) → override replaces.
    let base = yaml("x: scalar\n");
    let over = yaml("x:\n  nested: true\n");
    let merged = deep_merge(base, over);
    assert!(merged.get("x").unwrap().is_mapping());
}

#[test]
fn deep_merge_override_adds_new_key() {
    let base = yaml("a: 1\n");
    let over = yaml("b: 2\n");
    let merged = deep_merge(base, over);
    assert_eq!(merged.get("a").unwrap().as_i64(), Some(1));
    assert_eq!(merged.get("b").unwrap().as_i64(), Some(2));
}

// --- parse_compose_merged: behavior-preservation + roundtrip -----------------

#[test]
fn no_override_is_behavior_preserving() {
    // None override must equal parse_compose_with_scope over the base verbatim.
    let base = "services:\n  web:\n    image: nginx:1\n";
    let scope = VarScope::default();
    let via_merged = parse_compose_merged(base, None, &scope).unwrap();
    let via_direct = parse_compose_with_scope(base, &scope).unwrap();
    assert_eq!(via_merged.services.len(), via_direct.services.len());
    assert_eq!(
        via_merged.services[0].image_ref,
        via_direct.services[0].image_ref
    );
    assert_eq!(via_merged.services[0].image_ref, "nginx:1");
}

#[test]
fn merged_then_parse_roundtrip() {
    let base = "services:\n  web:\n    image: base:1\n  db:\n    image: pg:15\n";
    let over = "services:\n  web:\n    image: over:2\n";
    let scope = VarScope::default();
    let c = parse_compose_merged(base, Some(over), &scope).unwrap();

    let web = c.services.iter().find(|s| s.name == "web").unwrap();
    assert_eq!(web.image_ref, "over:2", "override image must win");
    let db = c.services.iter().find(|s| s.name == "db").unwrap();
    assert_eq!(
        db.image_ref, "pg:15",
        "base-only service must survive merge"
    );
}

#[test]
fn merged_list_replace_through_parse() {
    let base = "services:\n  web:\n    image: nginx:1\n    ports:\n      - \"8080:80\"\n      - \"9090:90\"\n";
    let over = "services:\n  web:\n    image: nginx:1\n    ports:\n      - \"3000:3000\"\n";
    let scope = VarScope::default();
    let c = parse_compose_merged(base, Some(over), &scope).unwrap();
    let web = c.services.iter().find(|s| s.name == "web").unwrap();
    assert_eq!(web.ports, vec![(3000u16, 3000u16)], "ports replaced");
}

#[test]
fn merged_propagates_base_parse_error() {
    // Fail-closed: malformed base surfaces as an honest error.
    let scope = VarScope::default();
    let err = parse_compose_merged("services: : :\n", Some("a: 1\n"), &scope);
    assert!(err.is_err());
}

#[test]
fn merged_propagates_override_parse_error() {
    let scope = VarScope::default();
    let err = parse_compose_merged("services:\n  web:\n    image: x\n", Some(": :\n"), &scope);
    assert!(err.is_err());
}

#[test]
fn override_filenames_match_docker_compose_order() {
    assert_eq!(
        OVERRIDE_FILENAMES,
        [
            "compose.override.yaml",
            "compose.override.yml",
            "docker-compose.override.yml",
        ]
    );
}
