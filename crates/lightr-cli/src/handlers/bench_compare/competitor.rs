//! Competitor-side measurements and spawn-guard dispatch for `bench-compare`.

use std::path::Path;
use std::process::Command;

use super::super::bench_compete_docker::{self as dp};
use super::model::{Cell, Detected, ProbePolicy, Runtime};

// ──────────────────────────────────────────────────────────────────────────────
// Competitor-side measurements
// ──────────────────────────────────────────────────────────────────────────────

/// Idle process count attributable to a competitor runtime — its daemon/VM
/// footprint while "installed but idle". Counts processes whose command mentions
/// the runtime's hallmark daemon names. Present runtime → a real count (often the
/// daemon+VM); absent → caller already produced SKIP, so this is only called for
/// present runtimes. `None` if `ps` is unavailable (honest NA).
pub(crate) fn competitor_idle_processes(rt: Runtime) -> Option<f64> {
    let needles: &[&str] = match rt {
        Runtime::Docker => &["dockerd", "docker", "com.docker", "vpnkit"],
        Runtime::OrbStack => &["orbstack", "OrbStack", "orbd", "orb"],
        Runtime::AppleContainer => &["container", "containerd"],
    };
    let out = Command::new("ps")
        .args(["-A", "-o", "comm="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut count = 0u64;
    for line in text.lines() {
        let comm = line.trim();
        if needles.iter().any(|n| comm.contains(n)) {
            count += 1;
        }
    }
    Some(count as f64)
}

// ──────────────────────────────────────────────────────────────────────────────
// Competitor spawn-guard + dispatch
// ──────────────────────────────────────────────────────────────────────────────

/// Map one competitor for a spawn-workload to a `Cell`, enforcing the spawn-guard
/// and the docker-only probe scope. `probe` is the per-workload Docker measurement
/// (resolved binary path + a scratch dir for its docker-side fixtures).
///
/// - absent on PATH ⇒ `Skip("absent on PATH")`
/// - present but `NeverSpawn` ⇒ `Skip` (test/CI guard — never spawns a container)
/// - present + `Spawn` + Docker ⇒ run the probe, map `Outcome` → `Cell`
/// - present + `Spawn` + non-Docker ⇒ `Skip` (only Docker has a probe today)
pub(crate) fn measure_competitor(
    d: &Detected,
    policy: ProbePolicy,
    scratch: &Path,
    probe: impl FnOnce(&Path, &Path) -> dp::Outcome,
) -> Cell {
    let Some(bin) = d.path.as_deref() else {
        return Cell::Skip("absent on PATH");
    };
    if policy == ProbePolicy::NeverSpawn {
        return Cell::Skip("competitor spawn disabled (test/CI tense-law guard)");
    }
    match d.runtime {
        Runtime::Docker => match probe(bin, scratch) {
            dp::Outcome::Measured(v) => Cell::Measured(v),
            dp::Outcome::Skip(r) => Cell::Skip(r),
        },
        _ => Cell::Skip("head-to-head probe implemented for docker only"),
    }
}
