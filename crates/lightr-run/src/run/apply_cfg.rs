//! RC-SEAM-FREEZE (skeleton-freeze) — the PER-FIELD runtime-config apply seam.
//!
//! Each new RC carry-field (`RunSpec`/`SpecOnDisk`) gets exactly ONE applier
//! here: `apply_<field>(value, &mut Command)`. Every applier is a NO-OP today
//! (it consumes its inputs and returns) so a run that sets none of these fields
//! behaves EXACTLY as before — behaviour-preserving. A future RC WP fills ONE
//! `apply_<field>` body (and sets the field from its CLI flag) touching ONLY its
//! own fn — the appliers are DISJOINT, so the fan-out never collides on the
//! native engine exec.
//!
//! Two thin dispatch entry points fan out to the SAME per-field appliers from
//! both native exec sites — the synchronous memo path (`RunSpec`, `memo.rs`)
//! and the detached supervisor (`SpecOnDisk`, `supervise_native.rs`) — so a
//! field filled here is honored on every native run with no further wiring.
//!
//! Cross-platform (template 8a): the Linux-only appliers
//! (caps/privileged/init/read-only/oom/pids/shm/tty) are `#[cfg(unix)]` on the
//! fn ITSELF and their dispatch calls are `#[cfg(unix)]`-gated, so they are not
//! dead code on the windows clippy gate. `hostname`/`labels` are platform-
//! neutral metadata and stay ungated.

use super::types::{RunSpec, SpecOnDisk};
use std::process::Command;

// ── Dispatch entry points ───────────────────────────────────────────────────

/// Apply the RC carry-fields of a `RunSpec` (synchronous native memo path) to
/// the child `Command`. All appliers are no-ops today (behaviour-preserving).
pub(super) fn apply_run_config_spec(spec: &RunSpec, cmd: &mut Command) {
    apply_hostname(spec.hostname.as_deref(), cmd);
    apply_labels(&spec.labels, cmd);
    #[cfg(unix)]
    {
        apply_cap_add(&spec.cap_add, cmd);
        apply_cap_drop(&spec.cap_drop, cmd);
        apply_privileged(spec.privileged, cmd);
        apply_tty(spec.tty, cmd);
        apply_init(spec.init, cmd);
        apply_read_only(spec.read_only, cmd);
        apply_oom_score_adj(spec.oom_score_adj, cmd);
        apply_pids_limit(spec.pids_limit, cmd);
        apply_shm_size(spec.shm_size, cmd);
    }
}

/// Apply the RC carry-fields of a `SpecOnDisk` (detached supervisor path) to the
/// child `Command`. Same per-field appliers as the `RunSpec` entry point — the
/// supervisor reads the persisted spec and honors the identical config.
pub(super) fn apply_run_config_ondisk(spec: &SpecOnDisk, cmd: &mut Command) {
    apply_hostname(spec.hostname.as_deref(), cmd);
    apply_labels(&spec.labels, cmd);
    #[cfg(unix)]
    {
        apply_cap_add(&spec.cap_add, cmd);
        apply_cap_drop(&spec.cap_drop, cmd);
        apply_privileged(spec.privileged, cmd);
        apply_tty(spec.tty, cmd);
        apply_init(spec.init, cmd);
        apply_read_only(spec.read_only, cmd);
        apply_oom_score_adj(spec.oom_score_adj, cmd);
        apply_pids_limit(spec.pids_limit, cmd);
        apply_shm_size(spec.shm_size, cmd);
    }
}

// ── Per-field appliers (one slot per future RC WP) ──────────────────────────
// STUBS: each is a no-op. Fill exactly one to land its flag's behaviour.

/// `--hostname`. No-op today. Platform-neutral (metadata).
fn apply_hostname(hostname: Option<&str>, cmd: &mut Command) {
    let _ = (hostname, cmd);
}

/// `--label`/`-l`. No-op today. Platform-neutral (metadata).
fn apply_labels(labels: &[(String, String)], cmd: &mut Command) {
    let _ = (labels, cmd);
}

/// `--cap-add`. No-op today (Linux capabilities).
#[cfg(unix)]
fn apply_cap_add(cap_add: &[String], cmd: &mut Command) {
    let _ = (cap_add, cmd);
}

/// `--cap-drop`. No-op today (Linux capabilities).
#[cfg(unix)]
fn apply_cap_drop(cap_drop: &[String], cmd: &mut Command) {
    let _ = (cap_drop, cmd);
}

/// `--privileged`. No-op today.
#[cfg(unix)]
fn apply_privileged(privileged: bool, cmd: &mut Command) {
    let _ = (privileged, cmd);
}

/// `-t`/`--tty`. No-op today.
#[cfg(unix)]
fn apply_tty(tty: bool, cmd: &mut Command) {
    let _ = (tty, cmd);
}

/// `--init`. No-op today (PID 1 zombie reaper).
#[cfg(unix)]
fn apply_init(init: bool, cmd: &mut Command) {
    let _ = (init, cmd);
}

/// `--read-only`. No-op today (read-only rootfs).
#[cfg(unix)]
fn apply_read_only(read_only: bool, cmd: &mut Command) {
    let _ = (read_only, cmd);
}

/// `--oom-score-adj`. No-op today.
#[cfg(unix)]
fn apply_oom_score_adj(oom_score_adj: Option<i32>, cmd: &mut Command) {
    let _ = (oom_score_adj, cmd);
}

/// `--pids-limit`. No-op today (cgroup `pids.max`).
#[cfg(unix)]
fn apply_pids_limit(pids_limit: Option<i64>, cmd: &mut Command) {
    let _ = (pids_limit, cmd);
}

/// `--shm-size`. No-op today (`/dev/shm` bytes).
#[cfg(unix)]
fn apply_shm_size(shm_size: Option<u64>, cmd: &mut Command) {
    let _ = (shm_size, cmd);
}

#[cfg(test)]
#[path = "apply_cfg_tests.rs"]
mod tests;
