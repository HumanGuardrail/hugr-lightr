//! Tests for the healthcheck probe + state machine (F-309 + WP-RC-4).
//! Split out via `#[path]` to keep healthcheck.rs under the 400-line godfile cap.

use super::*;

// ── probe (one-shot, retries-in-a-call) ──────────────────────────────────────

// probe returns Healthy for a command that exits 0.
#[test]
fn probe_healthy_on_success() {
    let tmp = tempfile::tempdir().unwrap();
    let hc = Healthcheck {
        cmd: "exit 0".to_string(),
        interval_s: 1,
        timeout_s: 0,
        start_period_s: 0,
        retries: 0,
    };
    assert_eq!(probe(&hc, tmp.path()), Health::Healthy);
}

// probe flips Healthy → Unhealthy on a failing command (all retries fail).
#[test]
fn probe_unhealthy_on_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let healthy = Healthcheck {
        cmd: "true".to_string(),
        interval_s: 1,
        timeout_s: 0,
        start_period_s: 0,
        retries: 2,
    };
    assert_eq!(
        probe(&healthy, tmp.path()),
        Health::Healthy,
        "a passing cmd must report Healthy"
    );

    let failing = Healthcheck {
        cmd: "exit 1".to_string(),
        interval_s: 1,
        timeout_s: 0,
        start_period_s: 0,
        retries: 2,
    };
    assert_eq!(
        probe(&failing, tmp.path()),
        Health::Unhealthy,
        "a cmd that always fails must report Unhealthy after retries"
    );
}

// ── probe_once + timeout ─────────────────────────────────────────────────────

// probe_once is a single round: success ⇒ true, failure ⇒ false.
#[test]
fn probe_once_single_round() {
    let tmp = tempfile::tempdir().unwrap();
    let ok = Healthcheck::new("exit 0".to_string());
    assert!(probe_once(&ok, tmp.path()), "exit 0 round passes");
    let bad = Healthcheck::new("exit 1".to_string());
    assert!(!probe_once(&bad, tmp.path()), "exit 1 round fails");
}

// A probe that outruns timeout_s is killed and counts as a failed round
// (Docker --health-timeout). `sleep 5` under a 1s timeout must fail fast.
#[cfg(unix)]
#[test]
fn probe_once_times_out() {
    let tmp = tempfile::tempdir().unwrap();
    let slow = Healthcheck {
        cmd: "sleep 5".to_string(),
        interval_s: 1,
        timeout_s: 1,
        start_period_s: 0,
        retries: 0,
    };
    let start = std::time::Instant::now();
    let passed = probe_once(&slow, tmp.path());
    let elapsed = start.elapsed();
    assert!(!passed, "a probe that outlives the timeout is a failure");
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "the timeout must cut the probe short, not wait the full sleep (took {elapsed:?})"
    );
}

// ── HealthState machine (the watchdog's core) ────────────────────────────────

// Cold start: machine begins in Starting with a zero streak.
#[test]
fn state_starts_in_starting() {
    let s = HealthState::default();
    assert_eq!(s.status, Health::Starting);
    assert_eq!(s.failing_streak, 0);
}

// starting → healthy on the first passing round.
#[test]
fn state_starting_to_healthy() {
    let mut s = HealthState::default();
    s.record(true, false, 3);
    assert_eq!(s.status, Health::Healthy);
    assert_eq!(s.failing_streak, 0);
}

// starting → unhealthy only AFTER retries+1 consecutive post-grace failures;
// each earlier failure keeps it Starting (not yet Unhealthy).
#[test]
fn state_to_unhealthy_after_retries() {
    let mut s = HealthState::default();
    let retries = 2; // tolerate 2 ⇒ flip on the 3rd consecutive failure
    s.record(false, false, retries);
    assert_eq!(s.status, Health::Starting, "1st failure: not yet unhealthy");
    assert_eq!(s.failing_streak, 1);
    s.record(false, false, retries);
    assert_eq!(s.status, Health::Starting, "2nd failure: still tolerated");
    assert_eq!(s.failing_streak, 2);
    s.record(false, false, retries);
    assert_eq!(
        s.status,
        Health::Unhealthy,
        "3rd consecutive failure breaches retries+1 ⇒ Unhealthy"
    );
    assert_eq!(s.failing_streak, 3);
}

// A success resets the failing streak (Docker FailingStreak semantics): a blip
// before the budget is exhausted does not accumulate toward Unhealthy.
#[test]
fn state_success_resets_streak() {
    let mut s = HealthState::default();
    s.record(false, false, 2);
    s.record(false, false, 2);
    assert_eq!(s.failing_streak, 2);
    s.record(true, false, 2);
    assert_eq!(s.failing_streak, 0, "success resets the streak");
    assert_eq!(s.status, Health::Healthy);
    // After the reset it must take a FRESH retries+1 run to go unhealthy.
    s.record(false, false, 2);
    assert_eq!(
        s.status,
        Health::Healthy,
        "1 failure post-reset is tolerated"
    );
}

// Failures INSIDE the start period accrue the streak but NEVER flip Unhealthy.
#[test]
fn state_start_period_never_unhealthy() {
    let mut s = HealthState::default();
    // Many failures, all in the grace window: stays Starting forever.
    for _ in 0..10 {
        s.record(false, true, 0);
    }
    assert_eq!(
        s.status,
        Health::Starting,
        "a failing probe in the start period must never go Unhealthy"
    );
    assert_eq!(
        s.failing_streak, 10,
        "the streak still accrues (Docker shows it)"
    );
    // The first post-grace failure then flips it (retries 0 ⇒ 1 failure).
    s.record(false, false, 0);
    assert_eq!(s.status, Health::Unhealthy);
}

// ── on-disk round-trips ──────────────────────────────────────────────────────

// write_state / read_state round-trip via the run dir's `health` file, incl.
// the new Starting state.
#[test]
fn health_state_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(read_state(tmp.path()), None, "no file ⇒ None");

    write_state(tmp.path(), Health::Starting);
    assert_eq!(read_state(tmp.path()), Some(Health::Starting));

    write_state(tmp.path(), Health::Healthy);
    assert_eq!(read_state(tmp.path()), Some(Health::Healthy));

    write_state(tmp.path(), Health::Unhealthy);
    assert_eq!(read_state(tmp.path()), Some(Health::Unhealthy));
}

// save_for / load_for round-trip the full Healthcheck config (all 5 fields).
#[test]
fn healthcheck_persist_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(load_for(tmp.path()).unwrap(), None, "no file ⇒ Ok(None)");

    let hc = Healthcheck {
        cmd: "curl -fsS localhost:8080/health".to_string(),
        interval_s: 15,
        timeout_s: 5,
        start_period_s: 10,
        retries: 5,
    };
    save_for(tmp.path(), &hc).unwrap();
    assert_eq!(load_for(tmp.path()).unwrap(), Some(hc));
}

// A pre-WP-RC-4 healthcheck.json (only cmd/interval/retries) still loads, with
// timeout/start-period taking the serde defaults (back-compat).
#[test]
fn healthcheck_load_legacy_defaults_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = r#"{"cmd":"true","interval_s":7,"retries":2}"#;
    std::fs::write(tmp.path().join("healthcheck.json"), legacy).unwrap();
    let loaded = load_for(tmp.path()).unwrap().expect("legacy must load");
    assert_eq!(loaded.cmd, "true");
    assert_eq!(loaded.interval_s, 7);
    assert_eq!(loaded.retries, 2);
    assert_eq!(loaded.timeout_s, 30, "missing timeout ⇒ Docker default 30s");
    assert_eq!(loaded.start_period_s, 0, "missing start-period ⇒ 0");
}
