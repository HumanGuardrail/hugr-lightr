//! CMP-P1-PROJECT tests: sanitization grammar + precedence resolution.
use super::*;
use std::path::Path;

#[test]
fn sanitize_lowercases_and_keeps_grammar() {
    assert_eq!(sanitize_project_name("MyApp").as_deref(), Some("myapp"));
    assert_eq!(sanitize_project_name("web-1_x").as_deref(), Some("web-1_x"));
    assert_eq!(sanitize_project_name("a").as_deref(), Some("a"));
    assert_eq!(sanitize_project_name("0").as_deref(), Some("0"));
}

#[test]
fn sanitize_drops_illegal_chars() {
    // dots/spaces/slashes are not in the alphabet ⇒ dropped.
    assert_eq!(sanitize_project_name("my.app").as_deref(), Some("myapp"));
    assert_eq!(sanitize_project_name("my app").as_deref(), Some("myapp"));
    assert_eq!(sanitize_project_name("a/b").as_deref(), Some("ab"));
    assert_eq!(sanitize_project_name("café").as_deref(), Some("caf"));
}

#[test]
fn sanitize_strips_leading_separators() {
    // first kept char must be [a-z0-9]; leading _/- are stripped.
    assert_eq!(sanitize_project_name("_web").as_deref(), Some("web"));
    assert_eq!(sanitize_project_name("--web").as_deref(), Some("web"));
    assert_eq!(sanitize_project_name("-_-app").as_deref(), Some("app"));
}

#[test]
fn sanitize_rejects_empty_result() {
    assert_eq!(sanitize_project_name(""), None);
    assert_eq!(sanitize_project_name("!!!"), None);
    assert_eq!(sanitize_project_name("___"), None);
    assert_eq!(sanitize_project_name("---"), None);
    assert_eq!(sanitize_project_name("   "), None);
}

#[test]
fn precedence_cli_wins() {
    let got = resolve_project_name(
        Some("CliName"),
        Some("envname"),
        Some("filename"),
        "dirname",
    )
    .unwrap();
    assert_eq!(got, "cliname");
}

#[test]
fn precedence_env_over_file_and_dir() {
    let got = resolve_project_name(None, Some("EnvName"), Some("filename"), "dirname").unwrap();
    assert_eq!(got, "envname");
}

#[test]
fn precedence_file_over_dir() {
    let got = resolve_project_name(None, None, Some("FileName"), "dirname").unwrap();
    assert_eq!(got, "filename");
}

#[test]
fn precedence_dir_basename_default() {
    let got = resolve_project_name(None, None, None, "MyProject").unwrap();
    assert_eq!(got, "myproject");
}

#[test]
fn explicit_invalid_is_fail_closed() {
    // cli/env/file that sanitize to nothing are honest errors (named source).
    let e = resolve_project_name(Some("!!!"), None, None, "ok").unwrap_err();
    assert!(
        format!("{e}").contains("--project-name"),
        "names cli source"
    );
    assert!(resolve_project_name(None, Some("___"), None, "ok").is_err());
    assert!(resolve_project_name(None, None, Some("   "), "ok").is_err());
}

#[test]
fn basename_fallback_degrades_to_default() {
    // a pathological cwd never aborts `compose up`.
    assert_eq!(
        resolve_project_name(None, None, None, "!!!").unwrap(),
        DEFAULT_PROJECT
    );
    assert_eq!(
        resolve_project_name(None, None, None, "").unwrap(),
        DEFAULT_PROJECT
    );
}

#[test]
fn dir_basename_extracts_final_component() {
    assert_eq!(dir_basename(Path::new("/home/me/MyApp")), "MyApp");
    assert_eq!(dir_basename(Path::new("relative/dir")), "dir");
    assert_eq!(dir_basename(Path::new("/")), "");
}
