//! WP-RC-RESTART — unit tests for the supervisor re-spawn DECISION: the
//! `should_restart` matrix, `policy_from_spec` (fail-closed), the crash-loop
//! backoff bound, and the explicit-stop marker. (Policy parse/validate is the
//! canonical `crate::restart::RestartPolicy`'s own tested concern.) Parallel-safe
//! (no process-global state; the marker test uses its own tempdir).
#![cfg(test)]

use super::{backoff_for, policy_from_spec, should_restart, BACKOFF_STEP, MAX_BACKOFF};
use crate::restart::RestartPolicy;

// ── policy_from_spec resolution ─────────────────────────────────────────────

#[test]
fn from_spec_none_is_no_and_bad_falls_closed_to_no() {
    assert_eq!(policy_from_spec(None), RestartPolicy::No);
    assert_eq!(policy_from_spec(Some("always")), RestartPolicy::Always);
    assert_eq!(
        policy_from_spec(Some("on-failure:2")),
        RestartPolicy::OnFailure { max: 2 }
    );
    // A corrupted on-disk policy must NOT silently crash-loop the child.
    assert_eq!(policy_from_spec(Some("garbage")), RestartPolicy::No);
}

// ── should_restart decision matrix ──────────────────────────────────────────

#[test]
fn no_policy_never_restarts() {
    assert!(!should_restart(RestartPolicy::No, 0, 0, false));
    assert!(!should_restart(RestartPolicy::No, 1, 0, false));
}

#[test]
fn always_restarts_on_any_exit() {
    assert!(should_restart(RestartPolicy::Always, 0, 0, false));
    assert!(should_restart(RestartPolicy::Always, 1, 7, false));
}

#[test]
fn unless_stopped_restarts_like_always_until_stopped() {
    let p = RestartPolicy::UnlessStopped;
    assert!(should_restart(p, 0, 0, false));
    assert!(should_restart(p, 1, 0, false));
    // An explicit stop disables restart.
    assert!(!should_restart(p, 1, 0, true));
}

#[test]
fn on_failure_restarts_only_on_nonzero_up_to_max() {
    let p = RestartPolicy::OnFailure { max: 2 };
    // zero exit ⇒ never restarts.
    assert!(!should_restart(p, 0, 0, false));
    // nonzero exit, under max ⇒ restarts.
    assert!(should_restart(p, 1, 0, false));
    assert!(should_restart(p, 1, 1, false));
    // at max ⇒ stops (2 restarts already done).
    assert!(!should_restart(p, 1, 2, false));
}

#[test]
fn on_failure_unbounded_when_max_zero() {
    // `max == 0` is the canonical bare `on-failure` (unbounded).
    let p = RestartPolicy::OnFailure { max: 0 };
    assert!(should_restart(p, 1, 1000, false));
    assert!(!should_restart(p, 0, 1000, false));
}

#[test]
fn explicit_stop_overrides_every_policy() {
    for p in [
        RestartPolicy::Always,
        RestartPolicy::UnlessStopped,
        RestartPolicy::OnFailure { max: 0 },
        RestartPolicy::OnFailure { max: 5 },
    ] {
        assert!(
            !should_restart(p, 1, 0, true),
            "an explicit stop must disable restart for {p:?}"
        );
    }
}

// ── backoff bound ───────────────────────────────────────────────────────────

#[test]
fn backoff_is_positive_and_bounded() {
    // Always > 0 so a crash-loop always sleeps (the "can't spin forever with no
    // delay" invariant).
    assert!(backoff_for(0) >= BACKOFF_STEP);
    assert!(backoff_for(1) >= BACKOFF_STEP);
    // Ramps with the attempt count.
    assert!(backoff_for(2) >= backoff_for(1));
    // Never exceeds the cap, even for a runaway crash-loop.
    assert!(backoff_for(10_000) <= MAX_BACKOFF);
    assert_eq!(backoff_for(10_000), MAX_BACKOFF);
}

// ── explicit-stop marker ────────────────────────────────────────────────────

#[test]
fn stop_marker_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!super::stop_requested(dir.path()), "no marker initially");
    super::write_stop_marker(dir.path());
    assert!(
        super::stop_requested(dir.path()),
        "marker present after write"
    );
}
