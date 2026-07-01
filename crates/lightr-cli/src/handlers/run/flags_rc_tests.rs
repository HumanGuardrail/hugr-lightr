//! WP-RC-FLAGS — `RawRcFlags::resolve` + the `--label`/`--shm-size` parsers.
//! Each flag flows raw clap value → resolved `RcConfig` field; bad `--label` /
//! `--shm-size` fail closed with exit 2. Parallel-safe (pure, no globals).

use super::{parse_label, parse_shm_size, RawRcFlags};

#[test]
fn label_parses_key_value() {
    assert_eq!(
        parse_label("env=prod").unwrap(),
        ("env".to_string(), "prod".to_string())
    );
}

#[test]
fn label_allows_empty_value() {
    assert_eq!(parse_label("k=").unwrap(), ("k".to_string(), String::new()));
}

#[test]
fn label_splits_on_first_eq() {
    assert_eq!(
        parse_label("k=a=b").unwrap(),
        ("k".to_string(), "a=b".to_string())
    );
}

#[test]
fn label_rejects_missing_eq() {
    assert_eq!(parse_label("nope").unwrap_err(), 2);
}

#[test]
fn label_rejects_empty_key() {
    assert_eq!(parse_label("=v").unwrap_err(), 2);
}

#[test]
fn shm_size_parses_units() {
    assert_eq!(parse_shm_size("64m").unwrap(), 64 * 1024 * 1024);
    assert_eq!(parse_shm_size("1g").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_shm_size("2048k").unwrap(), 2048 * 1024);
    assert_eq!(parse_shm_size("1048576").unwrap(), 1_048_576);
    assert_eq!(parse_shm_size("512b").unwrap(), 512);
}

#[test]
fn shm_size_rejects_zero_and_garbage() {
    assert_eq!(parse_shm_size("0").unwrap_err(), 2);
    assert_eq!(parse_shm_size("0m").unwrap_err(), 2);
    assert_eq!(parse_shm_size("abc").unwrap_err(), 2);
    assert_eq!(parse_shm_size("").unwrap_err(), 2);
}

/// All-default raw flags resolve to an all-default config (behaviour-preserving:
/// no flag set ⇒ no-op carry, every RunSpec field stays at its default).
#[test]
fn resolve_default_is_all_default() {
    let cfg = RawRcFlags::default().resolve().expect("default resolves");
    assert!(cfg.hostname.is_none());
    assert!(cfg.labels.is_empty());
    assert!(cfg.cap_add.is_empty());
    assert!(cfg.cap_drop.is_empty());
    assert!(!cfg.privileged);
    assert!(!cfg.tty);
    assert!(!cfg.init);
    assert!(!cfg.read_only);
    assert!(cfg.oom_score_adj.is_none());
    assert!(cfg.pids_limit.is_none());
    assert!(cfg.shm_size.is_none());
}

/// Every set flag flows to its resolved `RcConfig` field (the field-wiring
/// contract): labels parsed to pairs, shm-size to bytes, the rest passed through.
#[test]
fn resolve_threads_every_flag() {
    let raw = RawRcFlags {
        hostname: Some("h1".to_string()),
        label: vec!["a=1".to_string(), "b=2".to_string()],
        cap_add: vec!["NET_ADMIN".to_string()],
        cap_drop: vec!["MKNOD".to_string()],
        privileged: true,
        tty: true,
        init: true,
        read_only: true,
        oom_score_adj: Some(-250),
        pids_limit: Some(128),
        shm_size: Some("64m".to_string()),
        apparmor: Some("lightr-test-deny".to_string()),
        seccomp: Some("/tmp/lightr-test-seccomp.json".to_string()),
    };
    let cfg = raw.resolve().expect("resolves");
    assert_eq!(cfg.hostname.as_deref(), Some("h1"));
    assert_eq!(
        cfg.labels,
        vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string())
        ]
    );
    assert_eq!(cfg.cap_add, vec!["NET_ADMIN".to_string()]);
    assert_eq!(cfg.cap_drop, vec!["MKNOD".to_string()]);
    assert!(cfg.privileged && cfg.tty && cfg.init && cfg.read_only);
    assert_eq!(cfg.oom_score_adj, Some(-250));
    assert_eq!(cfg.pids_limit, Some(128));
    assert_eq!(cfg.shm_size, Some(64 * 1024 * 1024));
    // WP-#106: `--apparmor` threads through unparsed (profile name passthrough).
    assert_eq!(cfg.apparmor.as_deref(), Some("lightr-test-deny"));
    // WP-#108: `--seccomp` threads through unparsed (profile path passthrough).
    assert_eq!(
        cfg.seccomp.as_deref(),
        Some("/tmp/lightr-test-seccomp.json")
    );
}

/// A bad `--label` (or `--shm-size`) fails the whole resolve fail-closed (exit
/// 2), never silently dropping the flag.
#[test]
fn resolve_fails_closed_on_bad_flag() {
    let raw = RawRcFlags {
        label: vec!["nope".to_string()],
        ..Default::default()
    };
    assert_eq!(raw.resolve().unwrap_err(), 2);

    let raw2 = RawRcFlags {
        shm_size: Some("xyz".to_string()),
        ..Default::default()
    };
    assert_eq!(raw2.resolve().unwrap_err(), 2);
}
