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

// ---- CMP-P1-HEALTH-FULL: full compose healthcheck lowering ----

/// The lowered healthcheck tuple of the single service.
fn hc(c: &Compose) -> Option<LoweredHealthcheck> {
    c.services[0].healthcheck.clone()
}

#[test]
fn healthcheck_full_list_form_lowers_all_fields() {
    // test list (CMD form) + all four timing/count fields as durations.
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      test: [\"CMD\", \"curl\", \"-fsS\", \"localhost/health\"]\n      interval: 15s\n      timeout: 5s\n      start_period: 1m30s\n      retries: 5\n";
    let c = lower_yaml(yaml, None);
    assert_eq!(
        hc(&c),
        Some(("curl -fsS localhost/health".to_string(), 15, 5, 90, 5)),
        "(cmd, interval_s, timeout_s, start_period_s, retries)"
    );
}

#[test]
fn healthcheck_full_string_form_lowers_all_fields() {
    // test as a shell string; bare-integer durations ⇒ seconds.
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      test: pgrep myproc\n      interval: 30\n      timeout: 10\n      start_period: 0\n      retries: 2\n";
    let c = lower_yaml(yaml, None);
    assert_eq!(hc(&c), Some(("pgrep myproc".to_string(), 30, 10, 0, 2)));
}

#[test]
fn healthcheck_cmd_shell_list_form() {
    // CMD-SHELL list form strips the directive and joins the rest.
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      test: [\"CMD-SHELL\", \"exit 1\"]\n";
    let c = lower_yaml(yaml, None);
    let got = hc(&c).expect("healthcheck present");
    assert_eq!(got.0, "exit 1");
}

#[test]
fn healthcheck_docker_defaults_when_fields_absent() {
    // Only a command ⇒ Docker-faithful defaults (30s / 30s / 0s / 3).
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      test: pgrep x\n";
    let c = lower_yaml(yaml, None);
    assert_eq!(hc(&c), Some(("pgrep x".to_string(), 30, 30, 0, 3)));
}

#[test]
fn healthcheck_disable_true_drops() {
    // `disable: true` ⇒ no healthcheck even with a test present.
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      test: [\"CMD\", \"true\"]\n      disable: true\n";
    let c = lower_yaml(yaml, None);
    assert!(hc(&c).is_none(), "disable: true must drop the healthcheck");
}

#[test]
fn healthcheck_test_none_list_drops() {
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      test: [\"NONE\"]\n";
    let c = lower_yaml(yaml, None);
    assert!(hc(&c).is_none(), "test: [NONE] must drop the healthcheck");
}

#[test]
fn healthcheck_test_none_string_drops() {
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      test: NONE\n";
    let c = lower_yaml(yaml, None);
    assert!(hc(&c).is_none(), "test: NONE must drop the healthcheck");
}

#[test]
fn healthcheck_subset_back_compat() {
    // The pre-CMP-P1 subset (cmd alias + interval + retries) still lowers, now
    // with the RC-4 fields defaulted (timeout 30s, start_period 0s).
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      cmd: pgrep myproc\n      interval: 30\n      retries: 2\n";
    let c = lower_yaml(yaml, None);
    assert_eq!(hc(&c), Some(("pgrep myproc".to_string(), 30, 30, 0, 2)));
}

#[test]
fn no_healthcheck_unchanged() {
    let yaml = "services:\n  web:\n    image: x\n";
    let c = lower_yaml(yaml, None);
    assert!(hc(&c).is_none(), "no healthcheck declared ⇒ None");
}

// ---- SKELETON-FREEZE: frozen-but-not-yet-lowered aspects are no-ops ----

#[test]
fn frozen_aspects_lower_without_error_and_change_nothing() {
    // A service declaring EVERY frozen-but-not-yet-lowered field must still
    // lower cleanly (the per-aspect stubs are honest no-ops) and produce the
    // SAME runtime `Service` as the bare `image: x` service does today.
    let bare = "services:\n  web:\n    image: x\n";
    let full = "services:\n  web:\n    image: x\n    entrypoint: [\"/bin/init\"]\n    depends_on:\n      - db\n    deploy:\n      replicas: 3\n    networks:\n      - frontend\n    restart: always\n    extra_hosts:\n      - \"host:1.2.3.4\"\n    stop_grace_period: 10s\n    stop_signal: SIGINT\n    init: true\n    tty: true\n    cap_add:\n      - NET_ADMIN\n    cap_drop:\n      - ALL\n    privileged: true\n    container_name: my-web\n    working_dir: /app\n    user: \"1000\"\n";

    let b = lower_yaml(bare, None);
    let f = lower_yaml(full, None);

    let bs = &b.services[0];
    let fs = &f.services[0];
    // The stub aspects touch no runtime field ⇒ byte-identical Service.
    assert_eq!(bs.image_ref, fs.image_ref);
    assert_eq!(bs.command, fs.command);
    assert_eq!(bs.ports, fs.ports);
    assert_eq!(bs.env, fs.env);
    assert_eq!(bs.eager, fs.eager);
    assert_eq!(bs.secrets, fs.secrets);
    assert_eq!(bs.configs, fs.configs);
    assert_eq!(bs.healthcheck, fs.healthcheck);
}

#[test]
fn healthcheck_bad_duration_is_fail_closed() {
    let yaml = "services:\n  web:\n    image: x\n    healthcheck:\n      test: pgrep x\n      interval: 30x\n";
    let spec: ComposeSpec = serde_yaml::from_str(yaml).unwrap();
    let err = match lower(spec) {
        Ok(_) => panic!("expected bad-duration error"),
        Err(e) => e,
    };
    assert!(format!("{err}").contains("bad healthcheck interval"));
}

// --- CMP-P0-DEPENDS: depends_on lowering ---

use crate::build::compose::model::DepCondition;

/// The lowered depends_on edges of the named service.
fn deps_of<'a>(c: &'a Compose, name: &str) -> &'a [(String, DepCondition)] {
    &c.services
        .iter()
        .find(|s| s.name == name)
        .unwrap()
        .depends_on
}

#[test]
fn depends_on_short_form_defaults_to_started() {
    let yaml = "services:\n  web:\n    image: x\n    depends_on: [db, redis]\n  db:\n    image: d\n  redis:\n    image: r\n";
    let c = lower(serde_yaml::from_str(yaml).unwrap()).unwrap();
    assert_eq!(
        deps_of(&c, "web"),
        &[
            ("db".to_string(), DepCondition::Started),
            ("redis".to_string(), DepCondition::Started),
        ]
    );
}

#[test]
fn depends_on_long_form_carries_conditions() {
    let yaml = "services:\n  web:\n    image: x\n    depends_on:\n      db:\n        condition: service_healthy\n      migrate:\n        condition: service_completed_successfully\n  db:\n    image: d\n  migrate:\n    image: m\n";
    let c = lower(serde_yaml::from_str(yaml).unwrap()).unwrap();
    assert_eq!(
        deps_of(&c, "web"),
        &[
            ("db".to_string(), DepCondition::Healthy),
            ("migrate".to_string(), DepCondition::Completed),
        ]
    );
}

#[test]
fn depends_on_long_form_absent_condition_defaults_started() {
    let yaml =
        "services:\n  web:\n    image: x\n    depends_on:\n      db: {}\n  db:\n    image: d\n";
    let c = lower(serde_yaml::from_str(yaml).unwrap()).unwrap();
    assert_eq!(
        deps_of(&c, "web"),
        &[("db".to_string(), DepCondition::Started)]
    );
}

#[test]
fn no_depends_on_lowers_empty_edges() {
    let yaml = "services:\n  web:\n    image: x\n";
    let c = lower(serde_yaml::from_str(yaml).unwrap()).unwrap();
    assert!(deps_of(&c, "web").is_empty());
}

// --- CMP-LOWER-RUNCFG: working_dir / user / restart lowering ---

#[test]
fn run_config_fields_lower_onto_service() {
    // A service declaring working_dir/user/restart lowers each verbatim onto the
    // runtime Service (the supervisor then threads them into RunSpec).
    let yaml = "services:\n  web:\n    image: x\n    working_dir: /app\n    user: \"1000:1000\"\n    restart: on-failure:3\n";
    let c = lower_yaml(yaml, None);
    let s = &c.services[0];
    assert_eq!(s.working_dir.as_deref(), Some("/app"));
    assert_eq!(s.user.as_deref(), Some("1000:1000"));
    assert_eq!(s.restart.as_deref(), Some("on-failure:3"));
}

#[test]
fn run_config_fields_absent_lower_to_none() {
    // Behavior-preserving: a service without these fields lowers to None ⇒ the
    // supervisor's RunSpec keeps today's None placeholders.
    let yaml = "services:\n  web:\n    image: x\n";
    let c = lower_yaml(yaml, None);
    let s = &c.services[0];
    assert!(s.working_dir.is_none());
    assert!(s.user.is_none());
    assert!(s.restart.is_none());
}
