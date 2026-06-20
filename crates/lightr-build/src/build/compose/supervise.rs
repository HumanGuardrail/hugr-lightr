//! compose_supervise + helpers: start_service_detached, proxy_bidirectional, discovery_key,
//! prepare_service_cwd.
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::{Path, PathBuf};

use super::model::{DepCondition, ServiceSpec, StackSpec};
use super::up::lightr_home_pub as lightr_home;

/// CMP-P0-DEPENDS: cap (and poll interval) for a `service_healthy`/`_completed`
/// condition wait — fail-open after the cap so a never-healthy dep cannot wedge
/// the whole stack (a hung supervisor would violate the no-daemon discipline).
const DEP_WAIT_TIMEOUT_SECS: u64 = 60;
const DEP_POLL_INTERVAL_MS: u64 = 100;

/// CMP-P0-DEPENDS: order service indices so every `depends_on` dependency comes
/// before the service that depends on it (Kahn's algorithm, STABLE on the
/// declaration order for independent services). Returns the topological order,
/// or an honest error naming a cycle when one exists.
///
/// Behavior-preserving: a stack with NO `depends_on` edges has every in-degree
/// 0, so the queue drains in declaration order and the result is `0..n` — the
/// exact order the eager loop used before this WP. Edges to a service NOT in the
/// stack are ignored (a `depends_on` on an undeclared/external service does not
/// constrain ordering and must not be a phantom cycle).
pub(crate) fn topo_order(services: &[ServiceSpec]) -> Result<Vec<usize>> {
    let index_of: std::collections::HashMap<&str, usize> = services
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();

    let n = services.len();
    let mut indegree = vec![0usize; n];
    // adjacency: dep_index -> [dependent_index, ...]
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, svc) in services.iter().enumerate() {
        for (dep_name, _cond) in &svc.depends_on {
            if let Some(&dep_idx) = index_of.get(dep_name.as_str()) {
                dependents[dep_idx].push(i);
                indegree[i] += 1;
            }
        }
    }

    // Seed the queue with every in-degree-0 node in declaration order (stable).
    let mut queue: std::collections::VecDeque<usize> =
        (0..n).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(i) = queue.pop_front() {
        order.push(i);
        for &dependent in &dependents[i] {
            indegree[dependent] -= 1;
            if indegree[dependent] == 0 {
                queue.push_back(dependent);
            }
        }
    }

    if order.len() != n {
        // Whatever is left has a non-zero residual in-degree ⇒ a cycle. Name the
        // services still entangled so the error is actionable.
        let mut stuck: Vec<&str> = (0..n)
            .filter(|i| !order.contains(i))
            .map(|i| services[i].name.as_str())
            .collect();
        stuck.sort_unstable();
        return Err(LightrError::InvalidManifest(format!(
            "compose depends_on cycle among services: {}",
            stuck.join(", ")
        )));
    }
    Ok(order)
}

/// CMP-P0-DEPENDS: read a started dependency's verdict for one condition.
///
/// `service_started` ⇒ true the moment the dep has a run dir (it was spawned).
/// `service_healthy` ⇒ true once `<run_dir>/health` reports `healthy`.
/// `service_completed_successfully` ⇒ true once `<run_dir>/status` is `exited 0`.
/// Returns `false` while the verdict is not yet satisfiable (the caller polls).
fn dep_condition_met(run_dir: &Path, cond: DepCondition) -> bool {
    match cond {
        DepCondition::Started => true,
        DepCondition::Healthy => {
            lightr_run::healthcheck::read_state(run_dir)
                == Some(lightr_run::healthcheck::Health::Healthy)
        }
        DepCondition::Completed => std::fs::read_to_string(run_dir.join("status"))
            .map(|s| s.trim() == "exited 0")
            .unwrap_or(false),
    }
}

/// CMP-P0-DEPENDS: block until every `depends_on` edge of `svc` is satisfied (or
/// the wait times out — fail-open so a wedged dep cannot hang the supervisor).
/// Each dep's run dir is read live from `spec.json` (every `start_service_detached`
/// records it there), so a dep started earlier in the topo order is observable.
fn wait_for_deps(stack_dir: &Path, svc: &ServiceSpec) {
    if svc.depends_on.is_empty() {
        return;
    }
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(DEP_WAIT_TIMEOUT_SECS);
    for (dep_name, cond) in &svc.depends_on {
        loop {
            if let Some(run_dir) = dep_run_dir(stack_dir, dep_name) {
                if dep_condition_met(&run_dir, *cond) {
                    break;
                }
            }
            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "lightr compose: depends_on wait for {dep_name} ({cond:?}) timed out; starting {} anyway",
                    svc.name
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(DEP_POLL_INTERVAL_MS));
        }
    }
}

/// Read a dependency's run dir from the live `spec.json` (populated by
/// `start_service_detached` once the dep is spawned). `None` until the dep has
/// been started.
fn dep_run_dir(stack_dir: &Path, dep_name: &str) -> Option<PathBuf> {
    let bytes = std::fs::read(stack_dir.join("spec.json")).ok()?;
    let spec: StackSpec = serde_json::from_slice(&bytes).ok()?;
    spec.services
        .into_iter()
        .find(|s| s.name == dep_name)
        .and_then(|s| s.run_dir)
        .map(PathBuf::from)
}

/// Prepare a clean per-service run directory and, if the service declares an
/// `image_ref`, hydrate that ref's filesystem into it.
pub(crate) fn prepare_service_cwd(svc: &ServiceSpec, store: &Store) -> Result<PathBuf> {
    let cwd = std::env::temp_dir().join(format!("lightr-svc-{}", svc.name));
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
        // WP-RC-1 (R-KEY): compose service env is the UNKEYED DISCOVERY channel
        // (env_keys + child_env below) — it must NOT enter env_explicit, the
        // keyed user `-e`/`--env-file` channel. Left empty by design.
        env_explicit: Vec::new(),
        workdir: None, // compose working_dir lowering is a separate WP
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
