//! WP-CMP-NET tests: the named-networks ROUTING decision.
//!
//! Parallel-safe: pure in-memory `ServiceSpec` construction + routing; no
//! filesystem, no process-global state, no VM boot (full VM E2E is the on-box
//! follow-up).
use super::*;
use crate::build::compose::model::ServiceSpec;

/// A minimal `ServiceSpec` with the given name, image, and network attachments.
fn svc(name: &str, image: &str, networks: Vec<(&str, Vec<&str>)>) -> ServiceSpec {
    ServiceSpec {
        name: name.to_string(),
        image_ref: image.to_string(),
        command: vec!["/bin/true".to_string()],
        ports: Vec::new(),
        env: Vec::new(),
        eager: true,
        run_dirs: Vec::new(),
        run_dir: None,
        secrets: Vec::new(),
        configs: Vec::new(),
        healthcheck: None,
        depends_on: Vec::new(),
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
        networks: networks
            .into_iter()
            .map(|(n, a)| (n.to_string(), a.into_iter().map(String::from).collect()))
            .collect(),
        entrypoint: None,
        extra_hosts: Vec::new(),
        stop_signal: None,
        hostname: None,
    }
}

#[test]
fn declared_network_routes_to_vz_and_sets_runspec_network() {
    // The headline: a service on a network → vz engine, `RunSpec.network` set to
    // `<project>_<network>`, named after the SERVICE so the switch DNS resolves
    // `curl http://web`.
    let s = svc("web", "nginx-rootfs", vec![("frontend", vec![])]);
    let r = route_networking(&s, "myproj").unwrap();
    assert_eq!(r.engine, EngineKind::Vz);
    assert_eq!(r.network.as_deref(), Some("myproj_frontend"));
    assert_eq!(r.run_name_for_dns.as_deref(), Some("web"));
}

#[test]
fn two_services_on_shared_network_both_attach_under_their_names() {
    // A 2-service compose with a network → both get RunSpec.network set + would
    // attach. Same network id (shared) → they resolve EACH OTHER by name.
    let web = svc("web", "nginx-rootfs", vec![("appnet", vec![])]);
    let api = svc("api", "api-rootfs", vec![("appnet", vec![])]);
    let rw = route_networking(&web, "proj").unwrap();
    let ra = route_networking(&api, "proj").unwrap();
    assert_eq!(rw.engine, EngineKind::Vz);
    assert_eq!(ra.engine, EngineKind::Vz);
    assert_eq!(rw.network.as_deref(), Some("proj_appnet"));
    assert_eq!(ra.network.as_deref(), Some("proj_appnet"));
    assert_eq!(rw.run_name_for_dns.as_deref(), Some("web"));
    assert_eq!(ra.run_name_for_dns.as_deref(), Some("api"));
}

#[test]
fn aliases_become_network_alias() {
    // Long-form `aliases` ride through to RunSpec.network_alias (extra DNS names).
    let s = svc(
        "db",
        "pg-rootfs",
        vec![("backend", vec!["postgres", "primary"])],
    );
    let r = route_networking(&s, "proj").unwrap();
    assert_eq!(r.network_alias, vec!["postgres", "primary"]);
}

#[test]
fn networked_service_publishes_ports_to_guest() {
    // A networked (vz) service publishes its compose ports (svz forwarder); a
    // native one keeps ports empty (loopback proxy owns publishing).
    let mut s = svc("web", "nginx-rootfs", vec![("frontend", vec![])]);
    s.ports = vec![(8080, 80)];
    let r = route_networking(&s, "proj").unwrap();
    assert_eq!(r.ports.len(), 1);
    assert_eq!(r.ports[0].host, 8080);
    assert_eq!(r.ports[0].container, 80);
}

#[test]
fn no_network_routes_native_unchanged() {
    // A service on NO network → native, no RunSpec.network, empty ports/name —
    // byte-identical to today's loopback + env discovery path.
    let mut s = svc("plain", "anything", vec![]);
    s.ports = vec![(9090, 90)];
    let r = route_networking(&s, "proj").unwrap();
    assert_eq!(r.engine, EngineKind::Native);
    assert_eq!(r.network, None);
    assert!(r.network_alias.is_empty());
    assert!(r.run_name_for_dns.is_none());
    assert!(
        r.ports.is_empty(),
        "native publishing is the loopback proxy's job"
    );
}

#[test]
fn networked_service_without_image_is_fail_closed() {
    // A service that declares a network but has no rootfs image cannot host the
    // switch member → honest error, never a silent native fall-back.
    let empty_img = svc("web", "", vec![("frontend", vec![])]);
    assert!(route_networking(&empty_img, "proj").is_err());
    let scratch = svc("web", "scratch", vec![("frontend", vec![])]);
    assert!(route_networking(&scratch, "proj").is_err());
}

#[test]
fn first_network_chosen_when_multi_attached() {
    // Multiple networks → the FIRST is joined (one mesh NIC per vz run); the
    // routing still succeeds (the extras are noted, never a hard failure).
    let s = svc(
        "web",
        "rootfs",
        vec![("frontend", vec![]), ("backend", vec![])],
    );
    let r = route_networking(&s, "proj").unwrap();
    assert_eq!(r.network.as_deref(), Some("proj_frontend"));
}

#[test]
fn mesh_network_id_namespaces_by_project() {
    // Pure helper: `<project>_<network>` namespacing (Docker per-project nets).
    let s = svc("web", "rootfs", vec![("frontend", vec!["w"])]);
    let (id, aliases) = mesh_network_id(&s, "shop").unwrap();
    assert_eq!(id, "shop_frontend");
    assert_eq!(aliases, vec!["w"]);
    let plain = svc("p", "rootfs", vec![]);
    assert!(mesh_network_id(&plain, "shop").is_none());
}
