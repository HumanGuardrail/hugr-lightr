//! Tests for env folding + precedence in lowering (CMP-P0-ENVFILE-SVC).
//!
//! Parallel-safe: each test uses its own `tempfile::TempDir` for the env files
//! and goes through the base-dir-aware lowering entry. Tests here deliberately
//! avoid bare-key (process-env passthrough) lines — that rule is covered with an
//! injected lookup in `envfile_tests.rs` — so nothing reads process-global env.
use super::*;

/// Deserialize compose YAML and lower it against `base_dir`.
fn lower_yaml(yaml: &str, base_dir: Option<&std::path::Path>) -> Compose {
    let spec: ComposeSpec = serde_yaml::from_str(yaml).unwrap();
    lower_with_base_dir(spec, base_dir).unwrap()
}

/// The lowered env of the single service, as a sorted (k,v) vec for stable asserts.
fn sorted_env(c: &Compose) -> Vec<(String, String)> {
    let mut e = c.services[0].env.clone();
    e.sort();
    e
}

#[test]
fn single_env_file_folded() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("svc.env"), "FOO=fromfile\nBAR=baz\n").unwrap();
    let yaml = "services:\n  web:\n    image: x\n    env_file: svc.env\n";

    let c = lower_yaml(yaml, Some(dir.path()));
    assert_eq!(
        sorted_env(&c),
        vec![
            ("BAR".to_string(), "baz".to_string()),
            ("FOO".to_string(), "fromfile".to_string()),
        ]
    );
}

#[test]
fn inline_overrides_file() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("svc.env"), "FOO=fromfile\nONLYFILE=f\n").unwrap();
    let yaml = "services:\n  web:\n    image: x\n    env_file: svc.env\n    environment:\n      - FOO=inline\n      - ONLYINLINE=i\n";

    let c = lower_yaml(yaml, Some(dir.path()));
    assert_eq!(
        sorted_env(&c),
        vec![
            ("FOO".to_string(), "inline".to_string()),
            ("ONLYFILE".to_string(), "f".to_string()),
            ("ONLYINLINE".to_string(), "i".to_string()),
        ]
    );
}

#[test]
fn list_form_later_file_overrides_earlier() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.env"), "K=a\nA_ONLY=1\n").unwrap();
    std::fs::write(dir.path().join("b.env"), "K=b\nB_ONLY=2\n").unwrap();
    let yaml = "services:\n  web:\n    image: x\n    env_file:\n      - a.env\n      - b.env\n";

    let c = lower_yaml(yaml, Some(dir.path()));
    assert_eq!(
        sorted_env(&c),
        vec![
            ("A_ONLY".to_string(), "1".to_string()),
            ("B_ONLY".to_string(), "2".to_string()),
            ("K".to_string(), "b".to_string()), // later file wins
        ]
    );
}

#[test]
fn list_with_inline_on_top() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.env"), "K=a\n").unwrap();
    std::fs::write(dir.path().join("b.env"), "K=b\n").unwrap();
    let yaml = "services:\n  web:\n    image: x\n    env_file:\n      - a.env\n      - b.env\n    environment:\n      K: inline\n";

    let c = lower_yaml(yaml, Some(dir.path()));
    assert_eq!(
        sorted_env(&c),
        vec![("K".to_string(), "inline".to_string())]
    );
}

#[test]
fn missing_env_file_errors() {
    let dir = tempfile::TempDir::new().unwrap();
    let yaml = "services:\n  web:\n    image: x\n    env_file: nope.env\n";
    let spec: ComposeSpec = serde_yaml::from_str(yaml).unwrap();
    // `Compose` is not `Debug`, so match instead of `unwrap_err`.
    let err = match lower_with_base_dir(spec, Some(dir.path())) {
        Ok(_) => panic!("expected missing-env_file error"),
        Err(e) => e,
    };
    assert!(format!("{err}").contains("env_file not found"));
}

#[test]
fn comments_and_blanks_in_folded_file() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("svc.env"),
        "# header\n\nFOO=bar\n   \nBAZ=qux\n",
    )
    .unwrap();
    let yaml = "services:\n  web:\n    image: x\n    env_file: svc.env\n";

    let c = lower_yaml(yaml, Some(dir.path()));
    assert_eq!(
        sorted_env(&c),
        vec![
            ("BAZ".to_string(), "qux".to_string()),
            ("FOO".to_string(), "bar".to_string()),
        ]
    );
}

#[test]
fn no_env_file_is_behavior_preserving() {
    // Inline-only environment must lower exactly as the legacy `lower` path,
    // in declaration order (no override-collapsing applied).
    let yaml =
        "services:\n  web:\n    image: x\n    environment:\n      - A=1\n      - B=2\n      - A=3\n";
    let spec_legacy: ComposeSpec = serde_yaml::from_str(yaml).unwrap();
    let legacy = lower(spec_legacy).unwrap();

    let spec_new: ComposeSpec = serde_yaml::from_str(yaml).unwrap();
    let viadir = lower_with_base_dir(spec_new, Some(std::path::Path::new("/tmp"))).unwrap();

    assert_eq!(legacy.services[0].env, viadir.services[0].env);
    // Legacy preserves list order including the duplicate `A` (no collapse).
    assert_eq!(
        legacy.services[0].env,
        vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "3".to_string()),
        ]
    );
}

#[test]
fn no_env_file_no_environment_is_empty() {
    let yaml = "services:\n  web:\n    image: x\n";
    let c = lower_yaml(yaml, None);
    assert!(c.services[0].env.is_empty());
}
