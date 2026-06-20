//! HealthFlags::build (WP-RC-4) unit tests — split from run/tests.rs to keep
//! each file under the 400-LOC godfile cap (house convention).

use super::HealthFlags;

#[test]
fn health_flags_build_from_cmd() {
    // A --health-cmd with explicit timings lowers 1:1 to a Healthcheck.
    let flags = HealthFlags {
        cmd: Some("curl -fsS localhost/health".to_string()),
        interval: 15,
        timeout: 5,
        start_period: 10,
        retries: 4,
        no_healthcheck: false,
    };
    let hc = flags
        .build()
        .expect("a --health-cmd must build a Healthcheck");
    assert_eq!(hc.cmd, "curl -fsS localhost/health");
    assert_eq!(hc.interval_s, 15);
    assert_eq!(hc.timeout_s, 5);
    assert_eq!(hc.start_period_s, 10);
    assert_eq!(hc.retries, 4);
}

#[test]
fn health_flags_none_without_cmd() {
    // No --health-cmd ⇒ no healthcheck (the common case; behavior-preserving).
    let flags = HealthFlags {
        cmd: None,
        interval: 30,
        timeout: 30,
        start_period: 0,
        retries: 3,
        no_healthcheck: false,
    };
    assert!(flags.build().is_none(), "no --health-cmd ⇒ no healthcheck");
}

#[test]
fn health_flags_no_healthcheck_disables() {
    // --no-healthcheck wins even when --health-cmd is present (Docker
    // semantics: explicit disable beats a configured command).
    let flags = HealthFlags {
        cmd: Some("true".to_string()),
        interval: 30,
        timeout: 30,
        start_period: 0,
        retries: 3,
        no_healthcheck: true,
    };
    assert!(
        flags.build().is_none(),
        "--no-healthcheck must disable even with a --health-cmd"
    );
}

#[test]
fn health_flags_default_is_no_healthcheck() {
    // The Default (used by the docker-translation path + the no-flags run path)
    // builds no healthcheck — the behavior-preservation anchor.
    assert!(HealthFlags::default().build().is_none());
}
