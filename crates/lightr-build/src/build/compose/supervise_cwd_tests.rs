//! #75 FIX-2: the service cwd is namespaced by project so two projects with a
//! service of the same name never share — and thus never clobber — a cwd.
//!
//! Split from `supervise_tests.rs` for godfile headroom (house convention).
//! Parallel-safe: each test uses its own `TempDir`.
use super::*;
use lightr_store::Store;
use tempfile::TempDir;

/// A minimal command-only `ServiceSpec` with the given name (no image, no ports).
fn svc(name: &str) -> ServiceSpec {
    ServiceSpec {
        name: name.to_string(),
        image_ref: String::new(),
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
        networks: Vec::new(),
        entrypoint: None,
        extra_hosts: Vec::new(),
        stop_signal: None,
        hostname: None,
    }
}

#[test]
fn cwd_is_namespaced_by_project_no_cross_project_collision() {
    // Two projects each with a service named "web" must NOT share a cwd — the
    // pre-fix `lightr-svc-web` was unconditionally `remove_dir_all`'d, so project
    // B's `up` wiped project A's RUNNING cwd. Namespacing makes the paths differ.
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path()).unwrap();
    let s = svc("web");
    let names = replica_run_names(&s).unwrap();

    let cwd_a = prepare_service_cwd(&s, &store, &names[0], "proj-a").unwrap();
    let cwd_b = prepare_service_cwd(&s, &store, &names[0], "proj-b").unwrap();

    assert_ne!(
        cwd_a, cwd_b,
        "same service name across two projects must yield DISTINCT cwds"
    );
    assert!(cwd_a.to_string_lossy().contains("lightr-svc-proj-a-web"));
    assert!(cwd_b.to_string_lossy().contains("lightr-svc-proj-b-web"));
    // Preparing project B must NOT have wiped project A's cwd (still present).
    assert!(
        cwd_a.is_dir(),
        "project B's prepare must not clobber project A's running cwd"
    );
    let _ = std::fs::remove_dir_all(&cwd_a);
    let _ = std::fs::remove_dir_all(&cwd_b);
}
