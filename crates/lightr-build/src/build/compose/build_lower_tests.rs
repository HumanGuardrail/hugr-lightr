//! WP-E: unit tests for lowering `build:` (short + long, args map/list, target,
//! context anchoring). Pure — env is injected, so these are parallel-safe.
use super::*;
use crate::build::compose::spec::ComposeSpec;
use std::path::Path;

/// Parse a compose YAML and return the single named service's lowered build.
fn lower_svc_build(yaml: &str, base: Option<&Path>) -> Option<ServiceBuild> {
    let spec: ComposeSpec = serde_yaml::from_str(yaml).expect("parse compose");
    let def = spec.services.into_iter().next().expect("one service").1;
    let no_env = |_: &str| None;
    lower_build_with_env(def.build.as_ref(), base, &no_env).expect("lower build")
}

#[test]
fn short_form_is_context_with_default_dockerfile() {
    let b = lower_svc_build("services:\n  app:\n    build: ./ctx\n", None).unwrap();
    assert_eq!(b.context, "./ctx");
    assert_eq!(b.dockerfile, "Dockerfile");
    assert!(b.args.is_empty());
    assert_eq!(b.target, None);
}

#[test]
fn long_form_context_dockerfile_target() {
    let yaml = "services:\n  app:\n    build:\n      context: ./svc\n      dockerfile: Build.df\n      target: prod\n";
    let b = lower_svc_build(yaml, None).unwrap();
    assert_eq!(b.context, "./svc");
    assert_eq!(b.dockerfile, "Build.df");
    assert_eq!(b.target.as_deref(), Some("prod"));
}

#[test]
fn long_form_args_map() {
    let yaml = "services:\n  app:\n    build:\n      context: .\n      args:\n        VER: \"1.2\"\n        DEBUG: \"true\"\n";
    let b = lower_svc_build(yaml, None).unwrap();
    assert_eq!(
        b.args,
        vec![
            ("VER".to_string(), "1.2".to_string()),
            ("DEBUG".to_string(), "true".to_string())
        ]
    );
}

#[test]
fn long_form_args_list() {
    let yaml =
        "services:\n  app:\n    build:\n      context: .\n      args:\n        - VER=9\n        - NAME=lightr\n";
    let b = lower_svc_build(yaml, None).unwrap();
    assert_eq!(
        b.args,
        vec![
            ("VER".to_string(), "9".to_string()),
            ("NAME".to_string(), "lightr".to_string())
        ]
    );
}

#[test]
fn bare_arg_key_resolves_through_env_else_dropped() {
    let spec: ComposeSpec = serde_yaml::from_str(
        "services:\n  app:\n    build:\n      context: .\n      args:\n        - PRESENT\n        - ABSENT\n",
    )
    .unwrap();
    let def = spec.services.into_iter().next().unwrap().1;
    let env = |k: &str| (k == "PRESENT").then(|| "from-env".to_string());
    let b = lower_build_with_env(def.build.as_ref(), None, &env)
        .unwrap()
        .unwrap();
    assert_eq!(
        b.args,
        vec![("PRESENT".to_string(), "from-env".to_string())]
    );
}

#[test]
fn relative_context_anchored_against_base_dir() {
    let base = Path::new("/work/proj");
    let b = lower_svc_build("services:\n  app:\n    build: app\n", Some(base)).unwrap();
    assert_eq!(b.context, "/work/proj/app");
}

#[test]
fn absolute_context_unaffected_by_base_dir() {
    let base = Path::new("/work/proj");
    let b = lower_svc_build("services:\n  app:\n    build: /abs/ctx\n", Some(base)).unwrap();
    assert_eq!(b.context, "/abs/ctx");
}

#[test]
fn no_build_lowers_to_none() {
    let b = lower_svc_build("services:\n  app:\n    image: alpine\n", None);
    assert_eq!(b, None);
}

#[test]
fn empty_context_short_form_is_error() {
    let spec: ComposeSpec = serde_yaml::from_str("services:\n  app:\n    build: \"\"\n").unwrap();
    let def = spec.services.into_iter().next().unwrap().1;
    let no_env = |_: &str| None;
    assert!(lower_build_with_env(def.build.as_ref(), None, &no_env).is_err());
}

#[test]
fn long_form_missing_context_is_error() {
    let spec: ComposeSpec =
        serde_yaml::from_str("services:\n  app:\n    build:\n      dockerfile: D\n").unwrap();
    let def = spec.services.into_iter().next().unwrap().1;
    let no_env = |_: &str| None;
    assert!(lower_build_with_env(def.build.as_ref(), None, &no_env).is_err());
}
