//! SKELETON-FREEZE tests: a rich compose file exercising EVERY frozen
//! service-level + top-level field deserializes into `ComposeSpec` without
//! error and round-trips into the typed model. These assert SHAPE only (the
//! feature WPs assert lowering); they are pure deserialization, no I/O, so they
//! are parallel-safe.
use super::*;

/// One compose document touching every field added by the skeleton-freeze
/// (and the pre-existing ones), in both short and long polymorphic forms.
const RICH: &str = r#"
version: "3.9"
name: stackproj
services:
  web:
    image: nginx:latest
    build: .
    command: ["nginx", "-g", "daemon off;"]
    entrypoint: /docker-entrypoint.sh
    environment:
      FOO: bar
      BAZ: "1"
    env_file:
      - .env
    ports:
      - "8080:80"
      - target: 443
        published: 8443
        protocol: tcp
    expose:
      - 9000
    volumes:
      - "./data:/data"
    networks:
      - frontend
    depends_on:
      - db
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost"]
      interval: 30s
      retries: 3
    restart: unless-stopped
    deploy:
      replicas: 2
      resources:
        limits:
          cpus: "0.50"
          memory: 512M
        reservations:
          cpus: "0.25"
          memory: 256M
      restart_policy:
        condition: on-failure
        delay: 5s
        max_attempts: 3
        window: 120s
    profiles:
      - prod
    labels:
      com.example: "true"
    working_dir: /app
    user: "1000:1000"
    container_name: web1
    extends:
      file: common.yml
      service: base
    extra_hosts:
      - "host.docker.internal:host-gateway"
    stop_grace_period: 10s
    stop_signal: SIGTERM
    init: true
    tty: true
    cap_add:
      - NET_ADMIN
    cap_drop:
      - MKNOD
    privileged: false
    x-lightr-eager: true
    secrets:
      - db_password=ref:abc
    configs:
      - app_config=ref:def
  db:
    image: postgres:16
    networks:
      backend:
        aliases:
          - database
        ipv4_address: 10.0.0.5
    depends_on:
      web:
        condition: service_healthy
        required: true
networks:
  frontend: {}
  backend:
    driver: bridge
volumes:
  pgdata: {}
secrets:
  db_password:
    file: ./db_password.txt
configs:
  app_config:
    file: ./app_config.json
profiles:
  - prod
  - debug
"#;

#[test]
fn rich_compose_parses_into_model() {
    let spec: ComposeSpec = serde_yaml::from_str(RICH).expect("rich compose must parse");

    // Top-level.
    assert_eq!(spec.version.as_deref(), Some("3.9"));
    assert_eq!(spec.name.as_deref(), Some("stackproj"));
    assert_eq!(spec.services.len(), 2);
    assert!(spec.volumes.contains_key("pgdata"));
    assert!(spec.networks.contains_key("frontend"));
    assert!(spec.secrets.contains_key("db_password"));
    assert!(spec.configs.contains_key("app_config"));
    assert_eq!(spec.profiles, vec!["prod", "debug"]);

    let web = spec.services.get("web").expect("web service present");

    // depends_on short form.
    match web.depends_on.as_ref().expect("depends_on present") {
        DependsOn::List(l) => assert_eq!(l, &vec!["db".to_string()]),
        DependsOn::Map(_) => panic!("web depends_on is the short list form"),
    }

    // networks short form.
    match web.networks.as_ref().expect("networks present") {
        ServiceNetworks::List(l) => assert_eq!(l, &vec!["frontend".to_string()]),
        ServiceNetworks::Map(_) => panic!("web networks is the short list form"),
    }

    // deploy block.
    let deploy = web.deploy.as_ref().expect("deploy present");
    assert_eq!(deploy.replicas, Some(2));
    let limits = deploy
        .resources
        .as_ref()
        .and_then(|r| r.limits.as_ref())
        .expect("limits present");
    assert_eq!(limits.cpus.as_deref(), Some("0.50"));
    assert_eq!(limits.memory.as_deref(), Some("512M"));
    let rp = deploy
        .restart_policy
        .as_ref()
        .expect("restart_policy present");
    assert_eq!(rp.condition.as_deref(), Some("on-failure"));
    assert_eq!(rp.max_attempts, Some(3));

    // Scalar service-level freeze fields.
    assert_eq!(web.restart.as_deref(), Some("unless-stopped"));
    assert_eq!(web.working_dir.as_deref(), Some("/app"));
    assert_eq!(web.user.as_deref(), Some("1000:1000"));
    assert_eq!(web.container_name.as_deref(), Some("web1"));
    assert_eq!(web.stop_grace_period.as_deref(), Some("10s"));
    assert_eq!(web.stop_signal.as_deref(), Some("SIGTERM"));
    assert_eq!(web.init, Some(true));
    assert_eq!(web.tty, Some(true));
    assert_eq!(web.privileged, Some(false));
    assert_eq!(web.cap_add, vec!["NET_ADMIN".to_string()]);
    assert_eq!(web.cap_drop, vec!["MKNOD".to_string()]);
    assert!(web.extends.is_some());
    assert!(web.extra_hosts.is_some());
    assert_eq!(web.profiles, vec!["prod"]);
    assert_eq!(web.x_lightr_eager, Some(true));

    // db service: long forms of depends_on + networks.
    let db = spec.services.get("db").expect("db service present");
    match db.depends_on.as_ref().expect("db depends_on present") {
        DependsOn::Map(m) => {
            let e = m.get("web").expect("web dep entry");
            assert_eq!(e.condition.as_deref(), Some("service_healthy"));
            assert_eq!(e.required, Some(true));
        }
        DependsOn::List(_) => panic!("db depends_on is the long map form"),
    }
    match db.networks.as_ref().expect("db networks present") {
        ServiceNetworks::Map(m) => {
            let att = m
                .get("backend")
                .and_then(|o| o.as_ref())
                .expect("backend attachment");
            assert_eq!(att.aliases, vec!["database".to_string()]);
            assert_eq!(att.ipv4_address.as_deref(), Some("10.0.0.5"));
        }
        ServiceNetworks::List(_) => panic!("db networks is the long map form"),
    }
}

/// A minimal/partial file still parses (every field defaults) — leniency.
#[test]
fn partial_compose_defaults() {
    let spec: ComposeSpec = serde_yaml::from_str("services:\n  app:\n    image: x\n")
        .expect("partial compose must parse");
    let app = spec.services.get("app").unwrap();
    assert!(app.depends_on.is_none());
    assert!(app.deploy.is_none());
    assert!(app.networks.is_none());
    assert!(app.cap_add.is_empty());
    assert!(spec.secrets.is_empty());
    assert!(spec.profiles.is_empty());
}

/// Unknown keys are ignored (no `deny_unknown_fields`) — leniency preserved.
#[test]
fn unknown_keys_ignored() {
    let spec: ComposeSpec =
        serde_yaml::from_str("x-totally-unknown: 1\nservices:\n  a:\n    image: i\n    bogus: 9\n")
            .expect("unknown keys must be ignored");
    assert!(spec.services.contains_key("a"));
}
