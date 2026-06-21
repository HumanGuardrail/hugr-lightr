//! compose_supervise + helpers: start_service_detached, proxy_bidirectional, discovery_key,
//! prepare_service_cwd. The CMP-P0-DEPENDS topo-order + condition-wait helpers
//! live in the sibling `supervise_deps` module (godfile headroom for the
//! compose-lowering WPs that fill the `start_service_detached` RunSpec literal).
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::{Path, PathBuf};

use super::model::{ServiceSpec, StackSpec};
use super::supervise_deps::{topo_order, wait_for_deps};
use super::up::lightr_home_pub as lightr_home;

// Re-export the moved CMP-P0-DEPENDS helpers + the `DepCondition` they use so the
// sibling test module (`supervise_tests.rs`, `use super::*`) resolves them unchanged.
#[cfg(test)]
pub(crate) use super::model::DepCondition;
#[cfg(test)]
pub(crate) use super::supervise_deps::{dep_condition_met, dep_run_dir};

/// Prepare a clean per-service run directory and, if the service declares an
/// `image_ref`, hydrate that ref's filesystem into it.
pub(crate) fn prepare_service_cwd(svc: &ServiceSpec, store: &Store) -> Result<PathBuf> {
    // WP-CMP-CONFIG-LOWER: an explicit `container_name:` overrides the run-dir
    // name; absent ⇒ the service name (today's behavior). Only the materialized
    // dir is renamed — depends_on/discovery still key on `svc.name`.
    let run_name = svc.container_name.as_deref().unwrap_or(&svc.name);
    let cwd = std::env::temp_dir().join(format!("lightr-svc-{run_name}"));
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
/// `replicas > 1` (multi-instance spawn) is still a separate WP. Services with no
/// caps and no extra replicas produce no output (behavior-preserving).
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
    if let Some(n) = svc.replicas {
        if n > 1 {
            eprintln!(
                "lightr compose: service {:?}: deploy.replicas={n} requested but \
                 multi-instance spawn is not yet supported — starting a SINGLE \
                 instance (follow-up WP)",
                svc.name
            );
        }
    }
}

/// Spawn a service as a detached lightr run.
pub(crate) fn start_service_detached(
    stack_dir: &Path,
    svc: &ServiceSpec,
    peers: &[(String, u16)],
) -> Result<()> {
    use lightr_run::healthcheck::Healthcheck;
    use lightr_run::{spawn_detached_engine, Mount, RunSpec, StoreFile};

    let store_root = lightr_home().join("store");
    let store = Store::open(&store_root)?;
    let cwd = prepare_service_cwd(svc, &store)?;

    // WP-RESLIMITS: the deploy resource caps now FLOW to the supervisor — they are
    // carried on `RunSpec.limits` below → `SpecOnDisk` → `apply_native` at spawn
    // (memory is a hard RLIMIT_AS cap on Linux; cpus is recorded, honored under
    // vz). The note surfaces the cpu-recorded boundary + the still-unhonored
    // `replicas > 1` (multi-instance spawn is a separate WP); never a silent drop.
    // `restart` flows through the existing `RunSpec.restart` channel.
    note_unhonored_deploy(svc);

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
        ports: Vec::new(),
        // WP-RC-1 (R-KEY): compose env is the UNKEYED discovery channel, NOT keyed.
        env_explicit: Vec::new(),
        // CMP-LOWER-RUNCFG: lowered from the compose service (working_dir/user/
        // restart). `None` for a service that declares none ⇒ today's behavior
        // (run in cwd / current user / run-once).
        workdir: svc.working_dir.clone(),
        user: svc.user.clone(),
        restart: svc.restart.clone(),
        stop_signal: None, // WP-RC-STOPSIGNAL (NON-OWNED): compose stop_signal lowering is WP-RUNFLAGS' job.
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
        },
        // RC-SEAM-FREEZE (NON-OWNED site): remaining new RC fields (hostname/
        // labels/read_only/...) are future WPs' jobs → no-op defaults here.
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

    let handle = spawn_detached_engine(
        &spec,
        &store,
        hc.as_ref(),
        lightr_engine::EngineKind::Native,
        None,
        &child_env,
    )?;

    let spec_path = stack_dir.join("spec.json");
    if let Ok(bytes) = std::fs::read(&spec_path) {
        if let Ok(mut stack_spec) = serde_json::from_slice::<StackSpec>(&bytes) {
            for s in &mut stack_spec.services {
                if s.name == svc.name {
                    s.run_dir = Some(handle.dir.to_string_lossy().into_owned());
                }
            }
            if let Ok(new_bytes) = serde_json::to_vec_pretty(&stack_spec) {
                let _ = std::fs::write(&spec_path, &new_bytes);
            }
        }
    }

    Ok(())
}

/// Simple bidirectional byte proxy between two TCP streams.
pub(crate) fn proxy_bidirectional(a: std::net::TcpStream, b: std::net::TcpStream) {
    use std::io::{Read, Write};

    let a2 = a.try_clone();
    let b2 = b.try_clone();
    if a2.is_err() || b2.is_err() {
        return;
    }
    let mut a_read = a;
    let mut b_read = b;
    let mut a_write = a2.unwrap();
    let mut b_write = b2.unwrap();

    let t1 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match a_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if b_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let t2 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match b_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if a_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let _ = t1.join();
    let _ = t2.join();
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
    let order = topo_order(&spec.services)?;
    for &i in &order {
        let svc = &spec.services[i];
        if svc.eager && !svc.command.is_empty() {
            wait_for_deps(stack_dir, svc);
            start_service_detached(stack_dir, svc, &peers)?;
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
            let jh = std::thread::spawn(move || {
                if let Ok((inbound, _)) = listener.accept() {
                    if let Err(e) =
                        start_service_detached(&stack_dir_clone, &svc_clone, &peers_clone)
                    {
                        eprintln!("lightr compose: failed to start {}: {e}", svc_clone.name);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let svc_addr = format!("127.0.0.1:{container_port}");
                    if let Ok(outbound) = std::net::TcpStream::connect(&svc_addr) {
                        proxy_bidirectional(inbound, outbound);
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
