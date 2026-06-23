//! WP-REPLICAS: the pure `deploy.replicas` planning helpers for the compose
//! supervisor — how many instances to spawn, the static-port discriminator, and
//! the per-instance run-name plan (with the fail-closed rules). Split from
//! `supervise.rs` (godfile headroom); the actual spawn loop that consumes these
//! lives in `supervise.rs::start_service_detached`. These functions are pure
//! (no I/O, no process-global state) — the WP-REPLICAS tests assert on them.
use lightr_core::{LightrError, Result};

use super::model::ServiceSpec;

/// #75 FIX-2: sanitize a project name into a filesystem-safe cwd path segment.
///
/// The project is already resolved through `project::sanitize_project_name`
/// (grammar `[a-z0-9][a-z0-9_-]*`) at `compose up`, but this is total and
/// defensive: any char outside `[a-z0-9_-]` becomes `_`. An empty project, or one
/// where NO alphanumeric survives (e.g. `""`, `"///"`), degrades to `"default"`
/// (matching the project resolver's fallback) so the cwd is never the
/// un-namespaced `lightr-svc--<run_name>` nor a meaningless all-`_` segment.
pub(crate) fn sanitize_cwd_segment(project: &str) -> String {
    let out: String = project
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if out.chars().any(|c| c.is_ascii_alphanumeric()) {
        out
    } else {
        "default".to_string()
    }
}

/// WP-REPLICAS: how many instances of a service to spawn. `deploy.replicas`
/// defaults to 1 (Docker-faithful); a recorded `0` is clamped to 1 (a stack with
/// zero replicas would start nothing — we never silently drop the service, we run
/// the single-instance default). Absent ⇒ 1 (byte-identical to pre-replicas).
pub(crate) fn instance_count(svc: &ServiceSpec) -> u32 {
    svc.replicas.unwrap_or(1).max(1)
}

/// WP-REPLICAS: does this service publish a STATIC host port? `ports` holds
/// `(host_port, container_port)`; a host_port of `0` is "ephemeral / no static
/// publish". A non-zero host_port is a fixed published port — which cannot be
/// bound by more than one instance (the OS rejects the 2nd bind). This is the
/// discriminator for the replicas>1 fail-closed rule.
pub(crate) fn has_static_host_port(svc: &ServiceSpec) -> bool {
    svc.ports.iter().any(|&(host, _)| host != 0)
}

/// WP-REPLICAS: the per-instance run-NAMES for a service, honoring `deploy.replicas`.
///
/// TRANSCRIBE Docker Compose semantics:
/// * `replicas` absent / `1` ⇒ a SINGLE name — the explicit `container_name:` if
///   set, else the service name (byte-identical to the pre-replicas behavior).
/// * `replicas: N` (N>1) ⇒ N names `<service>_<i>` for i=1..=N (Compose's
///   per-replica suffix convention; Compose's full form is
///   `<project>_<service>_<i>`, but the project prefix is not carried on
///   `ServiceSpec` and the run-dir scheme here is `lightr-svc-<name>`, so the
///   minimal faithful per-instance name is `<service>_<i>`). An explicit
///   `container_name:` is INCOMPATIBLE with N>1 (Docker rejects a fixed container
///   name for a replicated service — it can't name N containers the same) and is
///   a fail-closed error.
/// * **Static published port + N>1 ⇒ fail-closed error.** A fixed host port can
///   be bound by exactly one process; Docker Compose itself fails the 2nd replica.
///   We refuse up front with an honest message rather than spawn-then-bind-fail.
///   (Replicas with NO static published port are fine — N instances spawn.)
///
/// NOTE (load-balancing gap): with N>1 the peer discovery env (WEB_HOST/WEB_PORT)
/// points at a SINGLE instance, not a round-robin set. Docker uses an embedded DNS
/// that round-robins the N replica IPs; that DNS/LB boundary is a Phase-2/vz
/// concern (same boundary as the compose DNS gap), noted here, not solved here.
pub(crate) fn replica_run_names(svc: &ServiceSpec) -> Result<Vec<String>> {
    let n = instance_count(svc);
    if n == 1 {
        // Byte-identical to today: container_name override, else service name.
        let run_name = svc.container_name.as_deref().unwrap_or(&svc.name);
        return Ok(vec![run_name.to_string()]);
    }
    if has_static_host_port(svc) {
        let port = svc
            .ports
            .iter()
            .find(|&&(host, _)| host != 0)
            .map(|&(host, _)| host)
            .unwrap_or(0);
        return Err(LightrError::InvalidManifest(format!(
            "service {:?}: published host port {port} cannot be published by {n} \
             replicas (a static host port binds exactly once); remove the static \
             host port to run replicas, or set replicas: 1",
            svc.name
        )));
    }
    if svc.container_name.is_some() {
        return Err(LightrError::InvalidManifest(format!(
            "service {:?}: container_name is incompatible with deploy.replicas={n} \
             (a fixed container name cannot be reused across {n} instances); \
             remove container_name to run replicas, or set replicas: 1",
            svc.name
        )));
    }
    Ok((1..=n).map(|i| format!("{}_{i}", svc.name)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `ServiceSpec` for the pure replica-planning tests (no I/O — these
    /// helpers never touch the filesystem or process-global state, so the tests are
    /// trivially parallel-safe).
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
    fn sanitize_cwd_segment_total_and_safe() {
        // #75 FIX-2: already-grammar projects pass through (lowercased); illegal
        // chars become `_`; an empty/all-illegal project degrades to "default"
        // (never the un-namespaced `lightr-svc--<run_name>`).
        assert_eq!(sanitize_cwd_segment("proj_a-1"), "proj_a-1");
        assert_eq!(sanitize_cwd_segment("My/Proj"), "my_proj");
        assert_eq!(sanitize_cwd_segment(""), "default");
        assert_eq!(sanitize_cwd_segment("///"), "default");
    }

    #[test]
    fn replicas_absent_is_single_instance_unchanged() {
        // Behavior-preserving: no replicas ⇒ exactly one instance, service name.
        let s = svc("web");
        assert_eq!(instance_count(&s), 1);
        assert_eq!(replica_run_names(&s).unwrap(), vec!["web".to_string()]);
    }

    #[test]
    fn replicas_one_is_single_instance_unchanged() {
        let mut s = svc("web");
        s.replicas = Some(1);
        assert_eq!(instance_count(&s), 1);
        assert_eq!(replica_run_names(&s).unwrap(), vec!["web".to_string()]);
    }

    #[test]
    fn replicas_zero_clamps_to_single_instance() {
        // A recorded 0 never silently drops the service — clamp to the default.
        let mut s = svc("web");
        s.replicas = Some(0);
        assert_eq!(instance_count(&s), 1);
        assert_eq!(replica_run_names(&s).unwrap(), vec!["web".to_string()]);
    }

    #[test]
    fn replicas_three_yields_three_named_instances() {
        // deploy.replicas: 3 (no static port) ⇒ web_1, web_2, web_3.
        let mut s = svc("web");
        s.replicas = Some(3);
        assert_eq!(instance_count(&s), 3);
        assert_eq!(
            replica_run_names(&s).unwrap(),
            vec![
                "web_1".to_string(),
                "web_2".to_string(),
                "web_3".to_string()
            ],
            "replicas:3 must produce 3 <service>_<i> instance names"
        );
    }

    #[test]
    fn replicas_with_ephemeral_port_is_fine() {
        // host_port 0 is NOT a static published port ⇒ replicas allowed.
        let mut s = svc("web");
        s.replicas = Some(2);
        s.ports = vec![(0, 8080)];
        assert!(!has_static_host_port(&s));
        assert_eq!(replica_run_names(&s).unwrap().len(), 2);
    }

    #[test]
    fn replicas_gt_one_with_static_port_is_honest_error() {
        // A static published host port binds once ⇒ replicas>1 fails closed.
        let mut s = svc("web");
        s.replicas = Some(3);
        s.ports = vec![(8080, 80)];
        assert!(has_static_host_port(&s));
        let msg = format!("{}", replica_run_names(&s).unwrap_err());
        assert!(
            msg.contains("8080") && msg.contains("replicas") && msg.contains("web"),
            "error must name the port + replicas + service; got: {msg}"
        );
    }

    #[test]
    fn static_port_with_single_replica_is_fine() {
        // The fail-closed rule is replicas>1 ONLY.
        let mut s = svc("web");
        s.ports = vec![(8080, 80)];
        assert_eq!(replica_run_names(&s).unwrap(), vec!["web".to_string()]);
        s.replicas = Some(1);
        assert_eq!(replica_run_names(&s).unwrap(), vec!["web".to_string()]);
    }

    #[test]
    fn replicas_gt_one_with_container_name_is_honest_error() {
        // A fixed container_name cannot name N instances ⇒ fail closed.
        let mut s = svc("web");
        s.replicas = Some(2);
        s.container_name = Some("fixed".to_string());
        let msg = format!("{}", replica_run_names(&s).unwrap_err());
        assert!(
            msg.contains("container_name") && msg.contains("web"),
            "error must name container_name + service; got: {msg}"
        );
    }
}
