//! Tests for the `compose up/down` handlers (split out for godfile headroom —
//! house convention `#[cfg(test)] #[path] mod tests;`). Parallel-safe: pure
//! helpers take injected paths; the few that touch `LIGHTR_HOME`/process env
//! hold `ENV_LOCK`.

use super::*;
use tempfile::TempDir;

/// `compose up` with a missing file ⇒ exit 1
#[test]
fn compose_up_missing_file_exits_1() {
    let code = up(
        "/no/such/file.yml",
        None,
        None,
        None,
        false,
        &[],
        3600,
        false,
    );
    assert_eq!(code, 1, "missing compose file must exit 1");
}

/// `compose up` with an empty services block ⇒ exit 0 (nothing to bind)
#[test]
fn compose_up_empty_services_exits_0() {
    let _env = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let f = tmp.path().join("compose.yml");
    std::fs::write(&f, "services:\n").unwrap();
    let code = up(
        f.to_str().unwrap(),
        None,
        None,
        None,
        false,
        &[],
        3600,
        false,
    );
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 0);
}

/// `compose down` with no active stack ⇒ exit 1
#[test]
fn compose_down_no_stack_exits_1() {
    let _env = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let code = down(None, None);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 1, "no active stack must exit 1");
}

/// resolve_latest_stack: returns None when compose dir is absent
#[test]
fn resolve_latest_stack_absent_dir_is_none() {
    let _env = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let result = resolve_latest_stack(None);
    std::env::remove_var("LIGHTR_HOME");
    assert!(result.is_none());
}

// ── CMP-P1-PROFILES: union_profiles (pure, env injected; parallel-safe) ──

#[test]
fn union_profiles_empty_when_nothing_given() {
    // Behavior-preserving: no --profile, no COMPOSE_PROFILES ⇒ empty union
    // ⇒ all services active downstream.
    assert!(union_profiles(&[], None).is_empty());
    assert!(union_profiles(&[], Some("")).is_empty());
}

#[test]
fn union_profiles_cli_only() {
    let cli = vec!["dev".to_string(), "debug".to_string()];
    assert_eq!(union_profiles(&cli, None), cli);
}

#[test]
fn union_profiles_env_only_comma_separated() {
    assert_eq!(
        union_profiles(&[], Some("dev,prod")),
        vec!["dev".to_string(), "prod".to_string()]
    );
}

#[test]
fn union_profiles_union_cli_and_env_dedup() {
    // CLI-first, env appended, duplicates dropped, blanks trimmed.
    let cli = vec!["dev".to_string()];
    assert_eq!(
        union_profiles(&cli, Some(" dev , prod , ")),
        vec!["dev".to_string(), "prod".to_string()]
    );
}

// ── CMP-CLI-INTEGRATION: the new flags parse through clap to the variant ──

#[test]
fn cli_parses_project_directory_and_env_file_flags() {
    use crate::cli::cmd::{Cli, Cmd, ComposeCmd};
    use clap::Parser as _;
    let cli = Cli::try_parse_from([
        "lightr",
        "compose",
        "up",
        "--project-directory",
        "/srv/app",
        "--env-file",
        "prod.env",
    ])
    .expect("flags parse");
    match cli.cmd {
        Cmd::Compose {
            subcmd:
                ComposeCmd::Up {
                    project_directory,
                    env_file,
                    ..
                },
        } => {
            assert_eq!(project_directory.as_deref(), Some("/srv/app"));
            assert_eq!(env_file.as_deref(), Some("prod.env"));
        }
        _ => panic!("expected compose up"),
    }
}

// ── CMP-CLI-INTEGRATION: project_directory (pure; --project-directory wins) ──

#[test]
fn project_directory_flag_overrides_compose_parent() {
    let d = project_directory("/some/where/compose.yml", Some("/elsewhere"));
    assert_eq!(d, PathBuf::from("/elsewhere"));
}

#[test]
fn project_directory_defaults_to_compose_parent() {
    let d = project_directory("/some/where/compose.yml", None);
    assert_eq!(d, PathBuf::from("/some/where"));
}

// ── CMP-CLI-INTEGRATION: --env-file dotenv subset (pure) ──

#[test]
fn parse_dotenv_subset_basic_and_quotes_and_export() {
    let text = "# comment\n\nexport A=1\nB = \"two words\"\nC='x'\nnoeq\n=bad\n";
    let pairs = parse_dotenv_subset(text);
    assert_eq!(
        pairs,
        vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "two words".to_string()),
            ("C".to_string(), "x".to_string()),
        ]
    );
}

#[test]
fn build_scope_missing_env_file_is_fail_closed() {
    // An explicit --env-file that cannot be read is an honest error.
    let err = build_scope(Path::new("/tmp"), Some("/no/such/.env"));
    assert!(
        err.is_err(),
        "unreadable --env-file must error, not silently skip"
    );
}

#[test]
fn build_scope_env_file_feeds_interpolation_process_env_wins() {
    let _env = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let ef = tmp.path().join("custom.env");
    std::fs::write(&ef, "CMP_ENVFILE_KEY=from_file\nCMP_OVERRIDDEN=file\n").unwrap();
    std::env::set_var("CMP_OVERRIDDEN", "process");
    let scope = build_scope(tmp.path(), Some(ef.to_str().unwrap())).unwrap();
    std::env::remove_var("CMP_OVERRIDDEN");
    // env-file value is present for interpolation…
    assert_eq!(
        scope.env.get("CMP_ENVFILE_KEY").map(String::as_str),
        Some("from_file")
    );
    // …and the live process env wins over the env-file (compose precedence).
    assert_eq!(
        scope.env.get("CMP_OVERRIDDEN").map(String::as_str),
        Some("process")
    );
}

// ── CMP-CLI-INTEGRATION: override discovery (beside the base, precedence) ──

#[test]
fn discover_override_finds_first_in_precedence() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().join("compose.yml");
    std::fs::write(&base, "services:\n").unwrap();
    // Only the second-precedence name exists ⇒ it is the one discovered.
    std::fs::write(tmp.path().join("compose.override.yml"), "services: {}\n").unwrap();
    let found = discover_override(base.to_str().unwrap());
    assert_eq!(found.as_deref(), Some("services: {}\n"));
}

#[test]
fn discover_override_none_when_absent() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().join("compose.yml");
    std::fs::write(&base, "services:\n").unwrap();
    assert!(discover_override(base.to_str().unwrap()).is_none());
}

// ── CMP-CLI-INTEGRATION: end-to-end-ish up exercises interp+env_file+merge+lowering ──

/// A full `compose up` over a tempdir: `--env-file` drives `${VAR}` interp, an
/// override file deep-merges over the base, and a lowering (`working_dir`) lands
/// in the written `spec.json`. Asserts the stack spec the supervisor will read.
#[test]
fn up_exercises_interp_envfile_merge_and_lowering() {
    let _env = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("LIGHTR_HOME", &home);

    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    // env-file supplies the image tag used via ${IMG_TAG} interpolation.
    let ef = proj.join("custom.env");
    std::fs::write(&ef, "IMG_TAG=fromenvfile\n").unwrap();
    // Base: image interpolated from the env-file; working_dir lowering present.
    let base = proj.join("compose.yml");
    std::fs::write(
        &base,
        "services:\n  web:\n    image: img:${IMG_TAG}\n    working_dir: /app\n",
    )
    .unwrap();
    // Override: deep-merge adds a second service over the base.
    std::fs::write(
        proj.join("compose.override.yml"),
        "services:\n  side:\n    image: side:latest\n",
    )
    .unwrap();

    let code = up(
        base.to_str().unwrap(),
        None,
        Some(proj.to_str().unwrap()),
        Some(ef.to_str().unwrap()),
        false,
        &[],
        3600,
        false,
    );
    assert_eq!(code, 0, "up must succeed");

    // Find the written stack spec and assert all three features landed.
    let compose_dir = home.join("compose");
    let stack = std::fs::read_dir(&compose_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("a stack dir was written");
    let spec_bytes = std::fs::read(stack.join("spec.json")).unwrap();
    let spec: lightr_build::StackSpec = serde_json::from_slice(&spec_bytes).unwrap();
    std::env::remove_var("LIGHTR_HOME");

    let by_name: std::collections::HashMap<&str, &_> =
        spec.services.iter().map(|s| (s.name.as_str(), s)).collect();
    // interpolation via --env-file applied to the image ref:
    let web = by_name.get("web").expect("web service present");
    assert_eq!(
        web.image_ref, "img:fromenvfile",
        "interp from --env-file must apply"
    );
    // a lowering reached the spec:
    assert_eq!(
        web.working_dir.as_deref(),
        Some("/app"),
        "working_dir lowering must land"
    );
    // override deep-merge pulled in the second service:
    assert!(
        by_name.contains_key("side"),
        "override-merge must add the override-only service"
    );
}
