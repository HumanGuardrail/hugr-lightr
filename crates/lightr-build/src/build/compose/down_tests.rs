//! Tests for `compose_down` — #75 FIX-1: every recorded replica run dir is
//! torn down (the pre-fix scalar field stopped only one, orphaning N-1).
//!
//! Parallel-safe: each test uses its own `TempDir`; the helpers are pure.
use super::*;
use std::fs;
use tempfile::TempDir;

/// A minimal `ServiceSpec` carrying the given recorded run dirs + legacy scalar.
fn svc_with_run_dirs(name: &str, run_dirs: Vec<&str>, legacy: Option<&str>) -> ServiceSpec {
    ServiceSpec {
        name: name.to_string(),
        image_ref: String::new(),
        command: vec!["/bin/true".to_string()],
        ports: Vec::new(),
        env: Vec::new(),
        eager: true,
        run_dirs: run_dirs.into_iter().map(|s| s.to_string()).collect(),
        run_dir: legacy.map(|s| s.to_string()),
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
fn compose_down_nonexistent_is_ok() {
    let tmp = TempDir::new().unwrap();
    let fake = tmp.path().join("no-such-stack");
    assert!(compose_down(&fake).is_ok());
}

#[test]
fn recorded_run_dirs_collects_every_replica() {
    // #75 FIX-1: deploy.replicas: 2 records TWO run dirs — both must be returned
    // for teardown (the pre-fix code returned only the first).
    let svc = svc_with_run_dirs("web", vec!["/run/web_1", "/run/web_2"], None);
    assert_eq!(
        recorded_run_dirs(&svc),
        vec!["/run/web_1".to_string(), "/run/web_2".to_string()],
        "both replica run dirs must be recorded for `compose down`"
    );
}

#[test]
fn recorded_run_dirs_folds_in_legacy_scalar() {
    // A stack `up`'d before this fix carried only the scalar `run_dir`; it must
    // still tear down (folded in), de-duplicated against `run_dirs`.
    let only_legacy = svc_with_run_dirs("api", vec![], Some("/run/api"));
    assert_eq!(
        recorded_run_dirs(&only_legacy),
        vec!["/run/api".to_string()]
    );

    let dup = svc_with_run_dirs("api", vec!["/run/api"], Some("/run/api"));
    assert_eq!(
        recorded_run_dirs(&dup),
        vec!["/run/api".to_string()],
        "a dir recorded in both places is stopped once, not twice"
    );
}

#[test]
fn compose_down_stops_both_replicas_and_removes_stack() {
    // End-to-end: a stack spec with a replicated service (two run dirs) → down
    // tears down BOTH instance dirs (no orphan survives) and removes the stack.
    let tmp = TempDir::new().unwrap();
    let stack_dir = tmp.path().join("stack");
    fs::create_dir_all(&stack_dir).unwrap();

    // Two real, distinct replica run dirs (so `dir.exists()` is true at down).
    let rd1 = tmp.path().join("run").join("web_1");
    let rd2 = tmp.path().join("run").join("web_2");
    fs::create_dir_all(&rd1).unwrap();
    fs::create_dir_all(&rd2).unwrap();

    let spec = StackSpec {
        ttl_secs: 60,
        created_at_unix: 0,
        project: "proj".to_string(),
        supervisor_pid: None,
        services: vec![svc_with_run_dirs(
            "web",
            vec![rd1.to_str().unwrap(), rd2.to_str().unwrap()],
            None,
        )],
    };
    fs::write(
        stack_dir.join("spec.json"),
        serde_json::to_vec_pretty(&spec).unwrap(),
    )
    .unwrap();

    // down visits BOTH recorded run dirs (stop is best-effort on a non-supervised
    // dir) and then removes the whole stack dir — proving no instance is skipped.
    assert!(compose_down(&stack_dir).is_ok());
    assert!(
        !stack_dir.exists(),
        "compose down must remove the stack dir after stopping every instance"
    );
}
