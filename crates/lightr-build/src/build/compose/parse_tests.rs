//! Tests for compose/parse.rs.
use super::*;

#[test]
fn parse_compose_two_services() {
    let yaml = "services:\n  web:\n    image: myimage\n    command: [\"sh\", \"-c\", \"echo hi\"]\n    ports:\n      - \"8080:80\"\n    environment:\n      - FOO=bar\n    x-lightr-eager: true\n  db:\n    image: dbimage\n    ports:\n      - \"5432:5432\"\n    environment:\n      - DB=test\n    unknown-key: ignored\n";
    let c = parse_compose(yaml).unwrap();
    assert_eq!(c.services.len(), 2);
    let web = &c.services[0];
    assert_eq!(web.name, "web");
    assert_eq!(web.image_ref, "myimage");
    assert_eq!(
        web.command,
        Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo hi".to_string()
        ])
    );
    assert_eq!(web.ports, vec![(8080u16, 80u16)]);
    assert_eq!(web.env, vec![("FOO".to_string(), "bar".to_string())]);
    assert!(web.eager);
    let db = &c.services[1];
    assert_eq!(db.name, "db");
    assert_eq!(db.image_ref, "dbimage");
    assert_eq!(db.ports, vec![(5432u16, 5432u16)]);
    assert_eq!(db.env, vec![("DB".to_string(), "test".to_string())]);
    assert!(!db.eager);
}

#[test]
fn parse_compose_unknown_key_ignored() {
    let yaml = "services:\n  svc:\n    image: foo\n    totally-unknown: whatever\n";
    let c = parse_compose(yaml).unwrap();
    assert_eq!(c.services.len(), 1);
    assert_eq!(c.services[0].image_ref, "foo");
}

#[test]
fn parse_compose_empty_services() {
    let yaml = "services:\n";
    let c = parse_compose(yaml).unwrap();
    assert_eq!(c.services.len(), 0);
}

#[test]
fn parse_compose_command_string_form() {
    let yaml = "services:\n  svc:\n    image: img\n    command: sleep 30\n";
    let c = parse_compose(yaml).unwrap();
    let cmd = c.services[0].command.as_ref().unwrap();
    assert_eq!(cmd, &["/bin/sh", "-c", "sleep 30"]);
}

#[test]
fn parse_compose_secrets_configs_healthcheck() {
    let yaml = "services:\n  api:\n    image: apiimg\n    command: serve\n    secrets:\n      - db_password=secret/db-pass\n      - api_key=secret/api-key\n    configs:\n      - app_conf=config/app\n    healthcheck:\n      test: [\"CMD\", \"curl\", \"-fsS\", \"localhost:8080/health\"]\n      interval: 15s\n      retries: 5\n";
    let c = parse_compose(yaml).unwrap();
    assert_eq!(c.services.len(), 1);
    let api = &c.services[0];
    assert_eq!(
        api.secrets,
        vec![
            ("db_password".to_string(), "secret/db-pass".to_string()),
            ("api_key".to_string(), "secret/api-key".to_string()),
        ]
    );
    assert_eq!(
        api.configs,
        vec![("app_conf".to_string(), "config/app".to_string())]
    );
    let hc = api.healthcheck.as_ref().expect("healthcheck parsed");
    // CMP-P1-HEALTH-FULL widened the tuple to
    // (cmd, interval_s, timeout_s, start_period_s, retries); the subset still
    // lowers cmd/interval/retries identically, with the RC-4 defaults filled in.
    assert_eq!(hc.0, "curl -fsS localhost:8080/health");
    assert_eq!(hc.1, 15);
    assert_eq!(hc.2, 30, "timeout_s defaults to 30s (RC-4)");
    assert_eq!(hc.3, 0, "start_period_s defaults to 0s (RC-4)");
    assert_eq!(hc.4, 5, "retries");
}

#[test]
fn parse_compose_healthcheck_string_form() {
    let yaml = "services:\n  svc:\n    image: i\n    healthcheck:\n      cmd: pgrep myproc\n      interval: 30\n      retries: 2\n";
    let c = parse_compose(yaml).unwrap();
    let hc = c.services[0].healthcheck.as_ref().expect("hc");
    // Widened tuple (CMP-P1-HEALTH-FULL): retries moved to index 4.
    assert_eq!(hc.0, "pgrep myproc");
    assert_eq!(hc.1, 30);
    assert_eq!(hc.4, 2, "retries");
}

#[test]
fn parse_compose_healthcheck_without_cmd_dropped() {
    let yaml = "services:\n  svc:\n    image: i\n    healthcheck:\n      interval: 10s\n";
    let c = parse_compose(yaml).unwrap();
    assert!(
        c.services[0].healthcheck.is_none(),
        "healthcheck without a command must be dropped"
    );
}

// ---- serde-model tests (CMP-P0-PARSER) ----

#[test]
fn parse_compose_environment_map_form() {
    let yaml = "services:\n  svc:\n    image: i\n    environment:\n      FOO: bar\n      NUM: 7\n      EMPTY:\n";
    let c = parse_compose(yaml).unwrap();
    let env = &c.services[0].env;
    // Numbers coerce to strings; empty/null map values are dropped (legacy).
    assert!(env.contains(&("FOO".to_string(), "bar".to_string())));
    assert!(env.contains(&("NUM".to_string(), "7".to_string())));
    assert!(
        !env.iter().any(|(k, _)| k == "EMPTY"),
        "empty map env value must be dropped"
    );
}

#[test]
fn parse_compose_environment_list_form() {
    let yaml = "services:\n  svc:\n    image: i\n    environment:\n      - FOO=bar\n      - BAZ=qux=extra\n";
    let c = parse_compose(yaml).unwrap();
    assert_eq!(
        c.services[0].env,
        vec![
            ("FOO".to_string(), "bar".to_string()),
            ("BAZ".to_string(), "qux=extra".to_string()),
        ]
    );
}

#[test]
fn parse_compose_preserves_service_order() {
    let yaml = "services:\n  zeta:\n    image: z\n  alpha:\n    image: a\n  mid:\n    image: m\n";
    let c = parse_compose(yaml).unwrap();
    let names: Vec<&str> = c.services.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["zeta", "alpha", "mid"],
        "declaration order kept"
    );
}

#[test]
fn parse_compose_rich_service_parses_without_error() {
    // depends_on, volumes, networks, deploy, profiles, labels, build, restart,
    // working_dir, user, container_name, expose, env_file, entrypoint — all
    // richer compose-spec fields must parse (ignored at lowering, consumed by
    // CMP-P1/P2) without erroring.
    let yaml = "version: \"3.9\"\nname: stack\nservices:\n  web:\n    image: nginx\n    entrypoint: [\"/entry.sh\"]\n    env_file: .env\n    expose:\n      - 9000\n    build:\n      context: .\n      dockerfile: Dockerfile\n    depends_on:\n      - db\n    volumes:\n      - data:/var/lib/data\n    networks:\n      - backend\n    deploy:\n      replicas: 2\n    profiles:\n      - prod\n    labels:\n      com.example: yes\n    restart: always\n    working_dir: /app\n    user: nobody\n    container_name: web1\n  db:\n    image: postgres\nvolumes:\n  data: {}\nnetworks:\n  backend: {}\n";
    let c = parse_compose(yaml).expect("rich compose parses");
    assert_eq!(c.services.len(), 2);
    assert_eq!(c.services[0].name, "web");
    assert_eq!(c.services[0].image_ref, "nginx");
}

#[test]
fn parse_compose_malformed_yaml_errors() {
    // Unbalanced flow mapping — genuinely malformed YAML.
    let yaml = "services:\n  svc:\n    image: [unterminated\n";
    assert!(
        parse_compose(yaml).is_err(),
        "malformed YAML must fail-closed, not panic"
    );
}

#[test]
fn parse_compose_top_level_unknown_ignored() {
    let yaml = "x-some-anchor: &a 1\ntotally-unknown: hi\nservices:\n  svc:\n    image: i\n";
    let c = parse_compose(yaml).unwrap();
    assert_eq!(c.services.len(), 1);
    assert_eq!(c.services[0].image_ref, "i");
}

#[test]
fn compose_spec_model_deserializes_both_env_forms() {
    use super::super::spec::{ComposeSpec, Environment};
    let yaml =
        "services:\n  a:\n    environment:\n      - K=V\n  b:\n    environment:\n      M: N\n";
    let spec: ComposeSpec = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(spec.services.len(), 2);
    assert!(matches!(
        spec.services["a"].environment,
        Some(Environment::List(_))
    ));
    assert!(matches!(
        spec.services["b"].environment,
        Some(Environment::Map(_))
    ));
}
