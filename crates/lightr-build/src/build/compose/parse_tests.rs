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
    assert_eq!(hc.0, "curl -fsS localhost:8080/health");
    assert_eq!(hc.1, 15);
    assert_eq!(hc.2, 5);
}

#[test]
fn parse_compose_healthcheck_string_form() {
    let yaml = "services:\n  svc:\n    image: i\n    healthcheck:\n      cmd: pgrep myproc\n      interval: 30\n      retries: 2\n";
    let c = parse_compose(yaml).unwrap();
    let hc = c.services[0].healthcheck.as_ref().expect("hc");
    assert_eq!(hc.0, "pgrep myproc");
    assert_eq!(hc.1, 30);
    assert_eq!(hc.2, 2);
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
