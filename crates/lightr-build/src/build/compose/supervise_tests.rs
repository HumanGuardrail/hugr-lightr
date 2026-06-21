//! Tests for the compose supervisor: service-cwd hydration + CMP-P0-DEPENDS
//! topological start ordering, cycle rejection, and condition verdicts.
//!
//! Parallel-safe: no process-global state; every test that touches the
//! filesystem uses its own `TempDir`.
use super::*;
use lightr_store::Store;
use tempfile::TempDir;

/// Build a minimal `ServiceSpec` with the given name and `depends_on` edges.
fn svc_with_deps(name: &str, deps: Vec<(&str, DepCondition)>) -> ServiceSpec {
    ServiceSpec {
        name: name.to_string(),
        image_ref: String::new(),
        command: vec!["/bin/true".to_string()],
        ports: Vec::new(),
        env: Vec::new(),
        eager: true,
        run_dir: None,
        secrets: Vec::new(),
        configs: Vec::new(),
        healthcheck: None,
        depends_on: deps.into_iter().map(|(n, c)| (n.to_string(), c)).collect(),
        working_dir: None,
        user: None,
        restart: None,
        mem_limit_bytes: None,
        cpu_limit_millis: None,
        replicas: None,
        init: false,
        tty: false,
        privileged: false,
        cap_add: Vec::new(),
        cap_drop: Vec::new(),
        container_name: None,
    }
}

/// The names in topo order (resolving indices back to names for readable asserts).
fn ordered_names(services: &[ServiceSpec]) -> Vec<String> {
    topo_order(services)
        .unwrap()
        .into_iter()
        .map(|i| services[i].name.clone())
        .collect()
}

#[test]
fn prepare_service_cwd_hydrates_image_ref() {
    let src = TempDir::new().unwrap();
    std::fs::write(src.path().join("marker.txt"), b"from-image").unwrap();
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path()).unwrap();
    lightr_index::snapshot(src.path(), &store, "svc-img").unwrap();
    let mut svc = svc_with_deps("hydrate-me", vec![]);
    svc.image_ref = "svc-img".to_string();
    let cwd = prepare_service_cwd(&svc, &store).unwrap();
    assert!(
        cwd.join("marker.txt").exists(),
        "image_ref file must be hydrated"
    );
    assert_eq!(
        std::fs::read(cwd.join("marker.txt")).unwrap(),
        b"from-image"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn prepare_service_cwd_empty_ref_is_clean() {
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path()).unwrap();
    let svc = svc_with_deps("cmd-only", vec![]);
    let cwd = prepare_service_cwd(&svc, &store).unwrap();
    assert!(cwd.is_dir());
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn topo_order_no_deps_preserves_declaration_order() {
    // Behavior-preserving: with no depends_on the order is 0..n (declaration).
    let services = vec![
        svc_with_deps("a", vec![]),
        svc_with_deps("b", vec![]),
        svc_with_deps("c", vec![]),
    ];
    assert_eq!(ordered_names(&services), vec!["a", "b", "c"]);
}

#[test]
fn topo_order_starts_dep_before_dependent() {
    // web depends_on db ⇒ db must come before web even though web is declared first.
    let services = vec![
        svc_with_deps("web", vec![("db", DepCondition::Started)]),
        svc_with_deps("db", vec![]),
    ];
    let order = ordered_names(&services);
    let db = order.iter().position(|n| n == "db").unwrap();
    let web = order.iter().position(|n| n == "web").unwrap();
    assert!(db < web, "db must start before web; got {order:?}");
}

#[test]
fn topo_order_transitive_chain() {
    // web -> api -> db. Result must be db, api, web.
    let services = vec![
        svc_with_deps("web", vec![("api", DepCondition::Started)]),
        svc_with_deps("api", vec![("db", DepCondition::Healthy)]),
        svc_with_deps("db", vec![]),
    ];
    assert_eq!(ordered_names(&services), vec!["db", "api", "web"]);
}

#[test]
fn topo_order_rejects_cycle() {
    // a -> b -> a is a cycle: honest error, not a partial order.
    let services = vec![
        svc_with_deps("a", vec![("b", DepCondition::Started)]),
        svc_with_deps("b", vec![("a", DepCondition::Started)]),
    ];
    let err = topo_order(&services).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("cycle") && msg.contains('a') && msg.contains('b'),
        "cycle error must name the entangled services; got: {msg}"
    );
}

#[test]
fn topo_order_ignores_edge_to_undeclared_service() {
    // depends_on an external/undeclared service is not a phantom cycle and does
    // not constrain ordering.
    let services = vec![svc_with_deps(
        "solo",
        vec![("ghost", DepCondition::Started)],
    )];
    assert_eq!(ordered_names(&services), vec!["solo"]);
}

#[test]
fn dep_condition_started_is_immediately_met() {
    // service_started: satisfied the moment the dep has a run dir (it spawned).
    let tmp = TempDir::new().unwrap();
    assert!(dep_condition_met(tmp.path(), DepCondition::Started));
}

#[test]
fn dep_condition_healthy_waits_on_health_verdict() {
    let tmp = TempDir::new().unwrap();
    // No health file yet ⇒ not met.
    assert!(!dep_condition_met(tmp.path(), DepCondition::Healthy));
    // A "starting" verdict is NOT healthy.
    lightr_run::healthcheck::write_state(tmp.path(), lightr_run::healthcheck::Health::Starting);
    assert!(!dep_condition_met(tmp.path(), DepCondition::Healthy));
    // Only "healthy" satisfies the gate.
    lightr_run::healthcheck::write_state(tmp.path(), lightr_run::healthcheck::Health::Healthy);
    assert!(dep_condition_met(tmp.path(), DepCondition::Healthy));
    // Unhealthy is not met.
    lightr_run::healthcheck::write_state(tmp.path(), lightr_run::healthcheck::Health::Unhealthy);
    assert!(!dep_condition_met(tmp.path(), DepCondition::Healthy));
}

#[test]
fn dep_condition_completed_waits_on_exit_zero() {
    let tmp = TempDir::new().unwrap();
    // No status file ⇒ not met.
    assert!(!dep_condition_met(tmp.path(), DepCondition::Completed));
    // Still running ⇒ not met.
    std::fs::write(tmp.path().join("status"), "running").unwrap();
    assert!(!dep_condition_met(tmp.path(), DepCondition::Completed));
    // Exited non-zero ⇒ NOT completed-successfully.
    std::fs::write(tmp.path().join("status"), "exited 1").unwrap();
    assert!(!dep_condition_met(tmp.path(), DepCondition::Completed));
    // Exited 0 ⇒ met.
    std::fs::write(tmp.path().join("status"), "exited 0").unwrap();
    assert!(dep_condition_met(tmp.path(), DepCondition::Completed));
}

#[test]
fn dep_run_dir_reads_live_spec() {
    // dep_run_dir resolves a started dep's run dir from the live spec.json.
    let stack = TempDir::new().unwrap();
    let mut db = svc_with_deps("db", vec![]);
    db.run_dir = Some("/tmp/run-db".to_string());
    let spec = StackSpec {
        ttl_secs: 60,
        created_at_unix: 0,
        project: "default".to_string(),
        supervisor_pid: None,
        services: vec![
            db,
            svc_with_deps("web", vec![("db", DepCondition::Started)]),
        ],
    };
    let bytes = serde_json::to_vec_pretty(&spec).unwrap();
    std::fs::write(stack.path().join("spec.json"), &bytes).unwrap();

    assert_eq!(
        dep_run_dir(stack.path(), "db"),
        Some(PathBuf::from("/tmp/run-db"))
    );
    // A not-yet-started dep has no run dir.
    assert_eq!(dep_run_dir(stack.path(), "web"), None);
    // An unknown service is None.
    assert_eq!(dep_run_dir(stack.path(), "nope"), None);
}

// --- CMP-LOWER-RUNCFG: working_dir/user/restart reach the spawned RunSpec ---

#[test]
fn run_config_fields_survive_spec_roundtrip_into_runspec() {
    // The supervisor reads each ServiceSpec back from spec.json, then
    // `start_service_detached` sets `RunSpec.workdir/user/restart` from the
    // ServiceSpec via `svc.<field>.clone()`. Assert the on-disk round-trip keeps
    // the fields so the RunSpec literal carries them (= the exact values fed to
    // the run side's WP-RC-WORKDIR/USER/RESTART honoring).
    let mut svc = svc_with_deps("web", vec![]);
    svc.working_dir = Some("/app".to_string());
    svc.user = Some("1000:1000".to_string());
    svc.restart = Some("on-failure:3".to_string());
    let spec = StackSpec {
        ttl_secs: 60,
        created_at_unix: 0,
        project: "default".to_string(),
        supervisor_pid: None,
        services: vec![svc],
    };
    let bytes = serde_json::to_vec_pretty(&spec).unwrap();
    let back: StackSpec = serde_json::from_slice(&bytes).unwrap();
    let s = &back.services[0];
    // These are exactly the sources the start_service_detached RunSpec literal
    // clones into workdir/user/restart.
    assert_eq!(s.working_dir.as_deref(), Some("/app"));
    assert_eq!(s.user.as_deref(), Some("1000:1000"));
    assert_eq!(s.restart.as_deref(), Some("on-failure:3"));
}

#[test]
fn run_config_absent_roundtrips_to_none() {
    // Behavior-preserving: a pre-CMP-LOWER-RUNCFG spec.json (no fields) loads as
    // None ⇒ the RunSpec literal keeps today's None placeholders.
    let legacy = r#"{"ttl_secs":60,"created_at_unix":0,"project":"default","supervisor_pid":null,"services":[{"name":"web","image_ref":"","command":["/bin/true"],"ports":[],"env":[],"eager":true,"run_dir":null}]}"#;
    let back: StackSpec = serde_json::from_str(legacy).unwrap();
    let s = &back.services[0];
    assert!(s.working_dir.is_none());
    assert!(s.user.is_none());
    assert!(s.restart.is_none());
}

#[test]
fn config_fields_survive_spec_roundtrip_into_runspec() {
    // WP-CMP-CONFIG-LOWER: the supervisor reads each ServiceSpec back and the
    // start_service_detached RunSpec literal sets init/tty/privileged/cap_add/
    // cap_drop from it. Assert the on-disk round-trip keeps them.
    let mut svc = svc_with_deps("web", vec![]);
    svc.init = true;
    svc.tty = true;
    svc.privileged = true;
    svc.cap_add = vec!["NET_ADMIN".to_string()];
    svc.cap_drop = vec!["MKNOD".to_string()];
    svc.container_name = Some("my-web".to_string());
    let spec = StackSpec {
        ttl_secs: 60,
        created_at_unix: 0,
        project: "default".to_string(),
        supervisor_pid: None,
        services: vec![svc],
    };
    let bytes = serde_json::to_vec_pretty(&spec).unwrap();
    let back: StackSpec = serde_json::from_slice(&bytes).unwrap();
    let s = &back.services[0];
    assert!(s.init);
    assert!(s.tty);
    assert!(s.privileged);
    assert_eq!(s.cap_add, vec!["NET_ADMIN".to_string()]);
    assert_eq!(s.cap_drop, vec!["MKNOD".to_string()]);
    assert_eq!(s.container_name.as_deref(), Some("my-web"));
}

#[test]
fn config_fields_absent_roundtrip_to_defaults() {
    // Behavior-preserving: a pre-WP-CMP-CONFIG-LOWER spec.json (no fields) loads
    // as false/empty/None ⇒ the RunSpec literal keeps today's no-op defaults.
    let legacy = r#"{"ttl_secs":60,"created_at_unix":0,"project":"default","supervisor_pid":null,"services":[{"name":"web","image_ref":"","command":["/bin/true"],"ports":[],"env":[],"eager":true,"run_dir":null}]}"#;
    let back: StackSpec = serde_json::from_str(legacy).unwrap();
    let s = &back.services[0];
    assert!(!s.init);
    assert!(!s.tty);
    assert!(!s.privileged);
    assert!(s.cap_add.is_empty());
    assert!(s.cap_drop.is_empty());
    assert!(s.container_name.is_none());
}

// --- WP-RESLIMITS: deploy.resources.limits reach the spawned RunSpec.limits ---

#[test]
fn deploy_limits_survive_spec_roundtrip_into_runspec() {
    // CMP-P1-DEPLOY lowers `deploy.resources.limits` onto `svc.mem_limit_bytes`/
    // `cpu_limit_millis`; the supervisor reads the ServiceSpec back and the
    // `start_service_detached` RunSpec literal maps them onto `RunSpec.limits`
    // (memory_bytes/cpu_millis) → SpecOnDisk → the supervisor's `apply_native`.
    // Assert the on-disk round-trip keeps both caps so the spawn carries them
    // (this is the #57 plumbing for the compose path: no longer silently dropped).
    let mut svc = svc_with_deps("web", vec![]);
    svc.mem_limit_bytes = Some(512 * 1024 * 1024);
    svc.cpu_limit_millis = Some(1500);
    let spec = StackSpec {
        ttl_secs: 60,
        created_at_unix: 0,
        project: "default".to_string(),
        supervisor_pid: None,
        services: vec![svc],
    };
    let bytes = serde_json::to_vec_pretty(&spec).unwrap();
    let back: StackSpec = serde_json::from_slice(&bytes).unwrap();
    let s = &back.services[0];
    // Exactly the sources the start_service_detached RunSpec.limits literal reads.
    assert_eq!(s.mem_limit_bytes, Some(512 * 1024 * 1024));
    assert_eq!(s.cpu_limit_millis, Some(1500));
    // And the mapping the literal performs yields the runtime caps the supervisor
    // reconstructs at spawn.
    let limits = lightr_core::ResourceLimits {
        memory_bytes: s.mem_limit_bytes,
        cpu_millis: s.cpu_limit_millis,
    };
    assert_eq!(limits.memory_bytes, Some(512 * 1024 * 1024));
    assert_eq!(limits.cpu_millis, Some(1500));
}

#[test]
fn deploy_limits_absent_roundtrip_to_unlimited() {
    // Behavior-preserving: a pre-WP-RESLIMITS spec.json (no limit fields) loads as
    // None ⇒ RunSpec.limits is unlimited ⇒ today's spawn, byte-identical.
    let legacy = r#"{"ttl_secs":60,"created_at_unix":0,"project":"default","supervisor_pid":null,"services":[{"name":"web","image_ref":"","command":["/bin/true"],"ports":[],"env":[],"eager":true,"run_dir":null}]}"#;
    let back: StackSpec = serde_json::from_str(legacy).unwrap();
    let s = &back.services[0];
    assert!(s.mem_limit_bytes.is_none());
    assert!(s.cpu_limit_millis.is_none());
    let limits = lightr_core::ResourceLimits {
        memory_bytes: s.mem_limit_bytes,
        cpu_millis: s.cpu_limit_millis,
    };
    assert!(
        limits.is_unlimited(),
        "absent caps ⇒ unlimited (unchanged spawn)"
    );
}

#[test]
fn container_name_overrides_run_dir_name() {
    // WP-CMP-CONFIG-LOWER: an explicit container_name renames the materialized
    // run dir; the service name is unchanged for depends_on/discovery.
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path()).unwrap();
    let mut svc = svc_with_deps("svc-name", vec![]);
    svc.container_name = Some("custom-run".to_string());
    let cwd = prepare_service_cwd(&svc, &store).unwrap();
    assert!(
        cwd.to_string_lossy().contains("lightr-svc-custom-run"),
        "container_name must drive the run-dir name, got {cwd:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn absent_container_name_uses_service_name_for_run_dir() {
    // Behavior-preserving: no container_name ⇒ the run dir is named from the
    // service name exactly as before.
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path()).unwrap();
    let svc = svc_with_deps("plain-svc", vec![]);
    let cwd = prepare_service_cwd(&svc, &store).unwrap();
    assert!(
        cwd.to_string_lossy().contains("lightr-svc-plain-svc"),
        "absent container_name must fall back to the service name, got {cwd:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}
