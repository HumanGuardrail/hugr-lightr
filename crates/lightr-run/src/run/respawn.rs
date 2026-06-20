//! WP-RC-RESTART — the detached supervisor's re-spawn DECISION + crash-loop
//! backoff + explicit-stop marker (the brain of `supervise_native`'s re-spawn
//! loop). The restart POLICY type itself is the canonical
//! [`crate::restart::RestartPolicy`] (F-308 — Docker `--restart`, already
//! parse/validate-complete); this module only adds the supervisor-runtime
//! behaviour around it. No new policy type is introduced.
//!
//! Restart is a RUNTIME parameter (like ports/workdir/user; like Docker, which
//! does not key on `--restart`) — it NEVER enters the memo key.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::restart::RestartPolicy;

/// Resolve an optional persisted policy string into a [`RestartPolicy`]. `None`
/// ⇒ [`RestartPolicy::No`] (a run with no `--restart` is byte-identical to
/// before). A `Some` that fails to parse fails closed to `No` — the policy was
/// already validated at the CLI guard, so an unparseable on-disk value is a
/// corrupted spec and must NOT silently crash-loop the child.
pub fn policy_from_spec(restart: Option<&str>) -> RestartPolicy {
    match restart {
        None => RestartPolicy::No,
        Some(s) => RestartPolicy::parse(s).unwrap_or(RestartPolicy::No),
    }
}

/// Decide whether the supervisor should re-spawn the child after it exited with
/// `exit_code`, given how many re-spawns have ALREADY happened (`restarts_done`)
/// and whether an explicit stop was requested (`stopped`).
///
/// An explicit stop ALWAYS wins (no re-spawn for any policy) — `lightr stop`/`rm`
/// must never trigger a restart. Otherwise, per [`RestartPolicy`]:
///   * `No` ⇒ never.
///   * `Always` / `UnlessStopped` ⇒ always (the stop check above differentiates
///     `UnlessStopped`).
///   * `OnFailure{max}` ⇒ only on a nonzero exit, while `restarts_done < max`
///     (`max == 0` ⇒ unbounded, matching the canonical bare `on-failure`).
pub fn should_restart(
    policy: RestartPolicy,
    exit_code: i32,
    restarts_done: u32,
    stopped: bool,
) -> bool {
    if stopped {
        return false;
    }
    match policy {
        RestartPolicy::No => false,
        RestartPolicy::Always | RestartPolicy::UnlessStopped => true,
        RestartPolicy::OnFailure { max } => exit_code != 0 && (max == 0 || restarts_done < max),
    }
}

/// Crash-loop backoff before a re-spawn. Bounds a tight crash-loop so a child
/// that exits instantly can't spin the supervisor at 100% CPU. A small linear
/// ramp capped at [`MAX_BACKOFF`] (cheap, predictable; the minimal Docker-faithful
/// choice that satisfies "can't spin forever with no delay").
pub const BACKOFF_STEP: Duration = Duration::from_millis(100);
/// The backoff cap (a re-spawn never waits longer than this).
pub const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// The delay before the `restarts_done`-th re-spawn (1-indexed): `step * n`,
/// capped at [`MAX_BACKOFF`]. Always > 0, so a crash-loop always sleeps.
pub fn backoff_for(restarts_done: u32) -> Duration {
    let n = restarts_done.max(1);
    let scaled = BACKOFF_STEP.saturating_mul(n);
    if scaled > MAX_BACKOFF {
        MAX_BACKOFF
    } else {
        scaled
    }
}

// ── Explicit-stop marker ────────────────────────────────────────────────────
// A sentinel file in the run dir. Written by the lifecycle/stop path (and by the
// supervisor's own ctl `signal` handler) the instant an EXPLICIT stop is
// requested, and polled by the re-spawn loop so an explicit `lightr stop`/`rm`
// (or a signal relayed through ctl.sock) never triggers a restart.

/// The stop-marker path for a run dir.
pub fn stop_marker_path(dir: &Path) -> PathBuf {
    dir.join("stop.requested")
}

/// Record that an EXPLICIT stop was requested for this run (best-effort: a write
/// failure must never abort the stop itself — the worst case is one extra
/// re-spawn, not a hang).
pub fn write_stop_marker(dir: &Path) {
    let _ = std::fs::write(stop_marker_path(dir), b"stopped");
}

/// Was an explicit stop requested for this run?
pub fn stop_requested(dir: &Path) -> bool {
    stop_marker_path(dir).exists()
}

#[cfg(test)]
#[path = "respawn_tests.rs"]
mod tests;
