//! CMP-P1-DEPLOY tests for `lower_deploy` + the `deploy`/top-level `restart`
//! precedence in `lower_restart`.
//!
//! Parallel-safe: pure in-memory deserialization + lowering; no filesystem,
//! no process-global state. Each test builds its own `ServiceDef` from a YAML
//! fragment and lowers it onto a fresh `empty_service`.
use super::*;
use crate::build::compose::model::empty_service;

/// Deserialize a single-service `deploy:`/`restart:` fragment into a `ServiceDef`.
fn def_from(yaml: &str) -> ServiceDef {
    serde_yaml::from_str(yaml).expect("ServiceDef YAML must parse")
}

/// Lower a fragment through BOTH `lower_deploy` and `lower_restart` in the same
/// order `lower.rs` dispatches them (deploy, then restart) so precedence is
/// exercised exactly as in production.
fn lower_deploy_then_restart(yaml: &str) -> Service {
    let def = def_from(yaml);
    let mut svc = empty_service("svc".to_string());
    lower_deploy(&def, &mut svc);
    lower_restart(&def, &mut svc);
    svc
}

#[test]
fn resource_limits_parse_like_run_flags() {
    // cpus "0.5" -> 500 millis; memory "512M" -> 512 MiB, same grammar as
    // `lightr run --cpus/--memory` (ResourceLimits::parse).
    let svc = lower_deploy_then_restart(
        "deploy:\n  resources:\n    limits:\n      cpus: \"0.5\"\n      memory: 512M\n",
    );
    assert_eq!(svc.cpu_limit_millis, Some(500));
    assert_eq!(svc.mem_limit_bytes, Some(512 * 1024 * 1024));
}

#[test]
fn memory_only_limit_leaves_cpu_unset() {
    let svc = lower_deploy_then_restart("deploy:\n  resources:\n    limits:\n      memory: 1g\n");
    assert_eq!(svc.mem_limit_bytes, Some(1024 * 1024 * 1024));
    assert_eq!(svc.cpu_limit_millis, None);
}

#[test]
fn malformed_limit_is_fail_loud_not_applied() {
    // Fail-closed: an unparseable cpus value is NOT cached as a bad number; the
    // cap is left unset (the lowering logs to stderr — not asserted here).
    let svc =
        lower_deploy_then_restart("deploy:\n  resources:\n    limits:\n      cpus: \"abc\"\n");
    assert_eq!(svc.cpu_limit_millis, None);
    assert_eq!(svc.mem_limit_bytes, None);
}

#[test]
fn restart_policy_condition_maps_to_docker_restart() {
    // any -> always, on-failure -> on-failure, none -> no.
    let any = lower_deploy_then_restart("deploy:\n  restart_policy:\n    condition: any\n");
    assert_eq!(any.restart.as_deref(), Some("always"));

    let onf = lower_deploy_then_restart("deploy:\n  restart_policy:\n    condition: on-failure\n");
    assert_eq!(onf.restart.as_deref(), Some("on-failure"));

    let none = lower_deploy_then_restart("deploy:\n  restart_policy:\n    condition: none\n");
    assert_eq!(none.restart.as_deref(), Some("no"));
}

#[test]
fn unknown_restart_condition_falls_back_to_no() {
    // Fail-closed: a typo'd condition never becomes a surprise auto-restart.
    let svc = lower_deploy_then_restart("deploy:\n  restart_policy:\n    condition: whenever\n");
    assert_eq!(svc.restart.as_deref(), Some("no"));
}

#[test]
fn deploy_restart_policy_wins_over_top_level_restart() {
    // Compose precedence: deploy.restart_policy.condition overrides `restart:`.
    let svc = lower_deploy_then_restart(
        "restart: always\ndeploy:\n  restart_policy:\n    condition: on-failure\n",
    );
    assert_eq!(
        svc.restart.as_deref(),
        Some("on-failure"),
        "deploy.restart_policy must win over top-level restart"
    );
}

#[test]
fn top_level_restart_used_when_deploy_has_no_policy() {
    // No restart_policy in deploy ⇒ top-level `restart:` is honored as before.
    let svc = lower_deploy_then_restart(
        "restart: unless-stopped\ndeploy:\n  resources:\n    limits:\n      memory: 64m\n",
    );
    assert_eq!(svc.restart.as_deref(), Some("unless-stopped"));
    assert_eq!(svc.mem_limit_bytes, Some(64 * 1024 * 1024));
}

#[test]
fn replicas_carried_for_honest_note_not_silently_dropped() {
    // OUT OF SCOPE: replicas>1 is recorded so the spawn site can warn; the
    // lowering does not half-spawn or silently ignore it.
    let svc = lower_deploy_then_restart("deploy:\n  replicas: 3\n");
    assert_eq!(svc.replicas, Some(3));
}

#[test]
fn no_deploy_is_behavior_preserving() {
    // No deploy + no restart ⇒ every deploy-derived field stays None.
    let svc = lower_deploy_then_restart("image: x\n");
    assert_eq!(svc.mem_limit_bytes, None);
    assert_eq!(svc.cpu_limit_millis, None);
    assert_eq!(svc.replicas, None);
    assert_eq!(svc.restart, None);
}

// ── WP-CMP-CONFIG-LOWER: runtime-config + capability lowering ───────────────

/// Lower a fragment through every WP-CMP-CONFIG-LOWER stub (init/tty/
/// container_name/cap_add/cap_drop/privileged) onto a fresh service, in the
/// dispatch grouping of `lower.rs` (runtime aspects then capability aspects).
fn lower_config(yaml: &str) -> Service {
    let def = def_from(yaml);
    let mut svc = empty_service("svc".to_string());
    lower_init(&def, &mut svc);
    lower_tty(&def, &mut svc);
    lower_container_name(&def, &mut svc);
    lower_cap_add(&def, &mut svc);
    lower_cap_drop(&def, &mut svc);
    lower_privileged(&def, &mut svc);
    svc
}

#[test]
fn config_fields_lower_into_service() {
    let svc = lower_config(
        "init: true\ntty: true\nprivileged: true\ncontainer_name: my-db\n\
         cap_add: [NET_ADMIN, SYS_TIME]\ncap_drop: [MKNOD]\n",
    );
    assert!(svc.init);
    assert!(svc.tty);
    assert!(svc.privileged);
    assert_eq!(svc.container_name.as_deref(), Some("my-db"));
    assert_eq!(svc.cap_add, vec!["NET_ADMIN", "SYS_TIME"]);
    assert_eq!(svc.cap_drop, vec!["MKNOD"]);
}

#[test]
fn absent_config_is_behavior_preserving() {
    // No init/tty/privileged/container_name/cap_* ⇒ the no-op defaults
    // (false/None/empty) ⇒ today's behavior, byte-identical.
    let svc = lower_config("image: x\n");
    assert!(!svc.init);
    assert!(!svc.tty);
    assert!(!svc.privileged);
    assert_eq!(svc.container_name, None);
    assert!(svc.cap_add.is_empty());
    assert!(svc.cap_drop.is_empty());
}

#[test]
fn explicit_false_bools_stay_false() {
    // `init: false` / `tty: false` / `privileged: false` are the absent default
    // and lower to false (no surprise toggle).
    let svc = lower_config("init: false\ntty: false\nprivileged: false\n");
    assert!(!svc.init);
    assert!(!svc.tty);
    assert!(!svc.privileged);
}
