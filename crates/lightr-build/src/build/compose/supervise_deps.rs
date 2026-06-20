//! CMP-P0-DEPENDS: `depends_on` topological start ordering + condition-wait
//! helpers for the compose supervisor. Split from `supervise.rs` (godfile
//! headroom): the topo-order (Kahn), per-condition verdict read, the blocking
//! wait, and the live run-dir lookup. Behavior is identical to the inlined form.
use lightr_core::{LightrError, Result};
use std::path::{Path, PathBuf};

use super::model::{DepCondition, ServiceSpec, StackSpec};

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
pub(crate) fn dep_condition_met(run_dir: &Path, cond: DepCondition) -> bool {
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
pub(crate) fn wait_for_deps(stack_dir: &Path, svc: &ServiceSpec) {
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
pub(crate) fn dep_run_dir(stack_dir: &Path, dep_name: &str) -> Option<PathBuf> {
    let bytes = std::fs::read(stack_dir.join("spec.json")).ok()?;
    let spec: StackSpec = serde_json::from_slice(&bytes).ok()?;
    spec.services
        .into_iter()
        .find(|s| s.name == dep_name)
        .and_then(|s| s.run_dir)
        .map(PathBuf::from)
}
