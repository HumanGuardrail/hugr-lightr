//! compose_supervise + helpers: start_service_detached, discovery_key,
//! prepare_service_cwd. The CMP-P0-DEPENDS topo-order + condition-wait helpers
//! live in the sibling `supervise_deps` module; the WP-CMP-NET routing decision
//! and the `proxy_bidirectional` TCP plumbing live in `supervise_net` (godfile
//! headroom for the compose-lowering WPs that fill the RunSpec literal).
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::{Path, PathBuf};

use super::model::{ServiceSpec, StackSpec};
use super::supervise_deps::{topo_order, wait_for_deps};
use super::supervise_replicas::{instance_count, replica_run_names, sanitize_cwd_segment};
use super::up::lightr_home_pub as lightr_home;

// Re-export the moved CMP-P0-DEPENDS helpers + the `DepCondition` they use so the
// sibling test module (`supervise_tests.rs`, `use super::*`) resolves them unchanged.
#[cfg(test)]
pub(crate) use super::model::DepCondition;
#[cfg(test)]
pub(crate) use super::supervise_deps::{dep_condition_met, dep_run_dir};

/// Prepare a clean per-service run directory and, if the service declares an
/// `image_ref`, hydrate that ref's filesystem into it.
///
/// `run_name` is the materialized run-dir name (WP-REPLICAS: one per replica
/// instance, computed by [`replica_run_names`]; for N=1 this is the
/// `container_name`-or-service-name as before).
pub(crate) fn prepare_service_cwd(
    svc: &ServiceSpec,
    store: &Store,
    run_name: &str,
    project: &str,
) -> Result<PathBuf> {
    // WP-CMP-CONFIG-LOWER: an explicit `container_name:` overrides the run-dir
    // name; absent ⇒ the service name (today's behavior). WP-REPLICAS: with N>1
    // each instance gets `<service>_<i>`. Only the materialized dir is renamed —
    // depends_on/discovery still key on `svc.name`.
    //
    // #75 FIX-2: namespace the cwd by PROJECT. Without it, two projects each with a
    // service named "web" share `lightr-svc-web`, and the unconditional
    // `remove_dir_all` below lets project B wipe project A's RUNNING cwd. The
    // project is sanitized to the same grammar service run-dir names use so the
    // path is always filesystem-safe.
    let cwd = std::env::temp_dir().join(format!(
        "lightr-svc-{}-{run_name}",
        sanitize_cwd_segment(project)
    ));
    if cwd.exists() {
        std::fs::remove_dir_all(&cwd).map_err(LightrError::Io)?;
    }
    std::fs::create_dir_all(&cwd).map_err(LightrError::Io)?;
    if !svc.image_ref.is_empty() && svc.image_ref != "scratch" {
        lightr_index::hydrate(&cwd, store, &svc.image_ref)?;
    }
    Ok(cwd)
}

/// WP-DISC: sanitize a compose service name into an env-var key prefix.
pub(crate) fn discovery_key(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// WP-RESLIMITS: emit an honest stderr note for the deploy aspects that are NOT
/// fully enforced on the native compose spawn path. `deploy.resources.limits` now
/// FLOW to the supervisor (via `RunSpec.limits` → `SpecOnDisk` → `apply_native`):
/// `memory` is a hard cap on Linux (`RLIMIT_AS`); `cpus` has no portable native
/// cpu-share cap so it is RECORDED only (honored under `--engine vz`). The note
/// surfaces the cpu-recorded-not-enforced boundary so it is never a silent drop.
/// WP-REPLICAS: `replicas > 1` now spawns N instances (handled in
/// [`start_service_detached`]); the note here surfaces the discovery
/// load-balancing gap (peers see ONE instance) so it is never silent. Services
/// with no caps and no extra replicas produce no output (behavior-preserving).
fn note_unhonored_deploy(svc: &ServiceSpec) {
    if svc.cpu_limit_millis.is_some() {
        eprintln!(
            "lightr compose: service {:?}: deploy.resources.limits.cpus \
             ({:?} millis) is RECORDED but not enforced as a cpu share on the \
             native engine (no portable native cap); use `--engine vz` for vcpu \
             caps. memory IS enforced on Linux (RLIMIT_AS).",
            svc.name, svc.cpu_limit_millis
        );
    }
    if instance_count(svc) > 1 {
        eprintln!(
            "lightr compose: service {:?}: deploy.replicas={} — spawning {} \
             instances; peer discovery (e.g. {}_HOST/_PORT) resolves to the FIRST \
             instance only (no DNS round-robin load-balancing across replicas — a \
             Phase-2/vz concern), never a silent drop.",
            svc.name,
            instance_count(svc),
            instance_count(svc),
            discovery_key(&svc.name)
        );
    }
}

/// Spawn a service as one-or-more detached lightr runs, honoring `deploy.replicas`.
///
/// WP-REPLICAS: N=1 (or absent) spawns a single instance with the existing run
/// name (byte-identical to today). N>1 spawns N instances named `<service>_<i>`
/// (after the fail-closed checks in [`replica_run_names`]).
///
/// #75 FIX-1: EVERY instance records its run dir on the live `spec.json` (the
/// pre-fix code recorded only instance 0, so `compose down` orphaned the other
/// N-1 replicas forever). depends_on/discovery still key on the service name and
/// resolve to the first instance (the LB note stands) — but teardown now sees all.
pub(crate) fn start_service_detached(
    stack_dir: &Path,
    svc: &ServiceSpec,
    peers: &[(String, u16)],
    project: &str,
) -> Result<()> {
    note_unhonored_deploy(svc);
    // Fail-closed BEFORE spawning anything (static-port/container_name + N>1).
    let run_names = replica_run_names(svc)?;
    for run_name in &run_names {
        start_one_instance(stack_dir, svc, peers, run_name, project)?;
    }
    Ok(())
}

/// Spawn exactly ONE instance of a service into `run_name`'s run dir.
fn start_one_instance(
    stack_dir: &Path,
    svc: &ServiceSpec,
    peers: &[(String, u16)],
    run_name: &str,
    project: &str,
) -> Result<()> {
    use super::supervise_net::{route_networking, NetRouting};
    use lightr_run::healthcheck::Healthcheck;
    use lightr_run::{spawn_detached_engine, Mount, RunSpec, StoreFile};

    let store_root = lightr_home().join("store");
    let store = Store::open(&store_root)?;

    // WP-CMP-NET: the hybrid routing decision — a declared-network service → vz +
    // the shared L2 switch (fail-closed if it has no image); a plain service →
    // native (byte-identical). See `supervise_net.rs`.
    let NetRouting {
        engine,
        ports,
        run_name_for_dns,
        network,
        network_alias,
    } = route_networking(svc, project)?;
    let cwd = prepare_service_cwd(svc, &store, run_name, project)?;

    let to_store_files = |pairs: &[(String, String)]| -> Vec<StoreFile> {
        pairs
            .iter()
            .map(|(name, ref_name)| StoreFile {
                name: name.clone(),
                ref_name: ref_name.clone(),
            })
            .collect()
    };

    let spec = RunSpec {
        cwd: cwd.clone(),
        inputs: Vec::new(),
        command: svc.command.clone(),
        env_keys: svc.env.iter().map(|(k, _)| k.clone()).collect(),
        mounts: Vec::new() as Vec<Mount>,
        secrets: to_store_files(&svc.secrets),
        configs: to_store_files(&svc.configs),
        ports,
        // WP-CMP-NET: vz path names the run after the SERVICE (switch/DNS member
        // name → `curl http://web`); `network` triggers C9's svz join + attach.
        // All default on the native path ⇒ byte-identical to today.
        name: run_name_for_dns,
        network,
        network_alias,
        // WP-RC-1 (R-KEY): compose env is the UNKEYED discovery channel, NOT keyed.
        env_explicit: Vec::new(),
        // CMP-LOWER-RUNCFG: lowered from the compose service (working_dir/user/
        // restart). `None` for a service that declares none ⇒ today's behavior
        // (run in cwd / current user / run-once).
        workdir: svc.working_dir.clone(),
        user: svc.user.clone(),
        restart: svc.restart.clone(),
        // WP-A: compose `stop_signal` lowered onto `RunSpec.stop_signal` (sent by
        // `lightr stop` before SIGKILL). Absent ⇒ `None` ⇒ SIGTERM (today's).
        stop_signal: svc.stop_signal.clone(),
        // WP-A: compose `entrypoint` lowered onto `RunSpec.entrypoint` (prepended
        // to `command` at exec). Absent ⇒ `None` ⇒ no override (today's).
        entrypoint: svc.entrypoint.clone(),
        // WP-A: compose `hostname` lowered onto `RunSpec.hostname`. Absent ⇒
        // `None` ⇒ no explicit hostname (today's).
        hostname: svc.hostname.clone(),
        // WP-A: compose `extra_hosts` lowered onto `RunSpec.add_host` (raw
        // `"host:ip"` strings; the vz wiring site parses them). Empty ⇒ none.
        add_host: svc.extra_hosts.clone(),
        // WP-CMP-CONFIG-LOWER: the RC-SEAM RunSpec fields lowered from the compose
        // service (init/tty/privileged/cap_add/cap_drop). All RUNTIME-ONLY (never
        // keyed). Absent on the service ⇒ false/empty here ⇒ today's behavior.
        init: svc.init,
        tty: svc.tty,
        privileged: svc.privileged,
        cap_add: svc.cap_add.clone(),
        cap_drop: svc.cap_drop.clone(),
        // WP-RESLIMITS: lower `deploy.resources.limits` (parsed in
        // `lower_resources::lower_deploy`) onto `RunSpec.limits` so they reach the
        // supervisor's `apply_native` (memory hard-capped on Linux; cpus recorded).
        // Absent ⇒ `None`/`None` (unlimited) ⇒ today's spawn, byte-identical.
        limits: lightr_core::ResourceLimits {
            memory_bytes: svc.mem_limit_bytes,
            cpu_millis: svc.cpu_limit_millis,
            // compose `deploy.resources` has no pids field; the native supervisor
            // cannot enforce pids anyway (cgroup-only) ⇒ never set here.
            pids_max: None,
        },
        // RC-SEAM-FREEZE (NON-OWNED site): remaining new RC fields (labels/
        // read_only/...) are future WPs' jobs → no-op defaults here.
        ..Default::default()
    };

    let mut child_env: Vec<(String, String)> = svc.env.clone();
    for (peer_name, container_port) in peers {
        if peer_name == &svc.name {
            continue;
        }
        let prefix = discovery_key(peer_name);
        child_env.push((format!("{prefix}_HOST"), "127.0.0.1".to_string()));
        child_env.push((format!("{prefix}_PORT"), container_port.to_string()));
    }

    // CMP-P1-HEALTH-FULL: compose now lowers the full healthcheck — cmd,
    // interval, timeout, start_period, retries — straight into the runtime
    // `Healthcheck` (the RC-4 fields are no longer hardcoded defaults).
    let hc =
        svc.healthcheck
            .as_ref()
            .map(
                |(cmd, interval_s, timeout_s, start_period_s, retries)| Healthcheck {
                    cmd: cmd.clone(),
                    interval_s: *interval_s,
                    timeout_s: *timeout_s,
                    start_period_s: *start_period_s,
                    retries: *retries,
                },
            );

    // WP-CMP-NET: vz boots the image as its rootfs (route_networking already
    // fail-closed-checked it is non-empty); native has none.
    let rootfs_ref = match engine {
        lightr_engine::EngineKind::Vz => Some(svc.image_ref.as_str()),
        _ => None,
    };

    let handle = spawn_detached_engine(&spec, &store, hc.as_ref(), engine, rootfs_ref, &child_env)?;

    // #75 FIX-1: APPEND this instance's run dir to the service's `run_dirs` on the
    // live spec — EVERY replica is recorded (not just instance 0), so `compose
    // down` stops all N. depends_on/discovery still resolve to the first recorded
    // instance (the LB note). Re-read → mutate → write keeps concurrent appends
    // from sibling replica spawns from clobbering each other (last-writer folds in
    // what it read; each instance spawns sequentially within a service here).
    let run_dir = handle.dir.to_string_lossy().into_owned();
    let spec_path = stack_dir.join("spec.json");
    if let Ok(bytes) = std::fs::read(&spec_path) {
        if let Ok(mut stack_spec) = serde_json::from_slice::<StackSpec>(&bytes) {
            for s in &mut stack_spec.services {
                if s.name == svc.name {
                    s.run_dirs.push(run_dir.clone());
                }
            }
            if let Ok(new_bytes) = serde_json::to_vec_pretty(&stack_spec) {
                let _ = std::fs::write(&spec_path, &new_bytes);
            }
        }
    }

    Ok(())
}

/// Compose supervisor -- called by `lightr __compose-supervise <stack_dir>`.
pub fn compose_supervise(stack_dir: &Path) -> Result<()> {
    use std::time::{Duration, Instant};

    let spec_path = stack_dir.join("spec.json");
    let spec_bytes = std::fs::read(&spec_path).map_err(LightrError::Io)?;
    let mut spec: StackSpec = serde_json::from_slice(&spec_bytes)
        .map_err(|e| LightrError::InvalidManifest(format!("stack spec parse: {e}")))?;

    let pid = std::process::id();
    std::fs::write(stack_dir.join("pid"), pid.to_string().as_bytes()).map_err(LightrError::Io)?;
    spec.supervisor_pid = Some(pid);
    let spec_bytes2 = serde_json::to_vec_pretty(&spec)
        .map_err(|e| LightrError::InvalidManifest(format!("serialize: {e}")))?;
    std::fs::write(&spec_path, &spec_bytes2).map_err(LightrError::Io)?;

    let ttl = Duration::from_secs(spec.ttl_secs);
    let start = Instant::now();
    let stop_file = stack_dir.join("stop");

    let peers: Vec<(String, u16)> = spec
        .services
        .iter()
        .filter_map(|s| {
            s.ports
                .first()
                .map(|&(_, container)| (s.name.clone(), container))
        })
        .collect();

    // CMP-P0-DEPENDS: start eager services in topological dependency order
    // (deps before dependents); a cycle is an honest error. Each eager service
    // waits out its `depends_on` conditions (service_started/healthy/completed)
    // before it is spawned. With no depends_on edges this is exactly the prior
    // declaration-order eager loop (topo_order returns 0..n; wait is a no-op).
    // WP-CMP-NET: project namespaces each network id (`<project>_<network>`).
    let project = spec.project.clone();

    let order = topo_order(&spec.services)?;
    for &i in &order {
        let svc = &spec.services[i];
        if svc.eager && !svc.command.is_empty() {
            wait_for_deps(stack_dir, svc);
            start_service_detached(stack_dir, svc, &peers, &project)?;
        }
    }

    let mut threads: Vec<std::thread::JoinHandle<()>> = Vec::new();

    for svc_spec in &spec.services {
        if svc_spec.eager {
            continue;
        }
        for &(host_port, container_port) in &svc_spec.ports {
            let addr = format!("127.0.0.1:{host_port}");
            let listener = match std::net::TcpListener::bind(&addr) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(
                        "lightr compose: bind {addr} for service {} failed: {e}",
                        svc_spec.name
                    );
                    continue;
                }
            };
            let svc_clone = svc_spec.clone();
            let stack_dir_clone = stack_dir.to_path_buf();
            let peers_clone = peers.clone();
            let project_clone = project.clone();
            let jh = std::thread::spawn(move || {
                if let Ok((inbound, _)) = listener.accept() {
                    if let Err(e) = start_service_detached(
                        &stack_dir_clone,
                        &svc_clone,
                        &peers_clone,
                        &project_clone,
                    ) {
                        eprintln!("lightr compose: failed to start {}: {e}", svc_clone.name);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let svc_addr = format!("127.0.0.1:{container_port}");
                    if let Ok(outbound) = std::net::TcpStream::connect(&svc_addr) {
                        super::supervise_net::proxy_bidirectional(inbound, outbound);
                    }
                }
            });
            threads.push(jh);
        }
    }

    loop {
        if stop_file.exists() || start.elapsed() >= ttl {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    Ok(())
}

// Tests live in a sibling file (godfile limit: this module's production code +
// the depends_on topo/wait logic exceeds the inline-tests budget). House
// convention — see network_tests.rs / imgmeta_tests.rs.
#[cfg(test)]
#[path = "supervise_tests.rs"]
mod tests;

// #75 FIX-2: the project-namespaced-cwd tests live in their own sibling (godfile
// headroom; `supervise_tests.rs` is at the budget).
#[cfg(test)]
#[path = "supervise_cwd_tests.rs"]
mod cwd_tests;
