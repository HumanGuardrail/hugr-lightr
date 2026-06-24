//! WP-NET-ISO: unit tests for `--net` parsing + the native+`--net=none` honest
//! error. These are pure-policy tests (no netns, no spawn) so the macOS gate
//! stays green; the real CLONE_NEWNET behavior is Linux-cloud-only.

use super::flags::net_isolate_from_str;
use super::{run, HealthFlags};

// ── --net parse (host default / none accepted / bad value ⇒ error) ──────────

#[test]
fn net_host_is_not_isolated() {
    // `host` (the default) ⇒ share the host network (net_isolate=false).
    assert_eq!(net_isolate_from_str("host"), Ok(false));
}

#[test]
fn net_none_is_isolated() {
    // `none` ⇒ isolated netns (net_isolate=true).
    assert_eq!(net_isolate_from_str("none"), Ok(true));
}

#[test]
fn net_bad_value_errors_exit_2() {
    // Any other value is fail-closed: honest error + exit 2.
    assert_eq!(net_isolate_from_str("bridge"), Err(2));
    assert_eq!(net_isolate_from_str(""), Err(2));
    assert_eq!(net_isolate_from_str("None"), Err(2));
}

// ── native + --net=none ⇒ honest error (no netns possible) ──────────────────

/// `--net=none` on the pure-native path (native engine, no rootfs) has no netns
/// to create, so the handler must exit 2 with the honest "requires --engine ns
/// or vz" error — BEFORE any store/engine work. (`host` on the same path runs
/// normally; this guards only the isolation request.)
#[test]
fn native_net_none_exits_2() {
    let code = run(
        ".",
        &[],
        &[],
        &["true".to_string()],
        false, // json
        false, // explain
        false, // detach
        &[],   // publish
        false, // publish_all
        &[],   // mounts
        "native",
        None,   // rootfs (pure native — no netns possible)
        "none", // net (WP-NET-ISO) — isolation requested
        false,  // deep_memo
        None,   // memory
        None,   // cpus
        &[],    // secrets
        &[],    // configs
        &[],    // env_set
        None,   // env_file
        None,   // workdir
        None,   // user
        None,   // restart
        None,   // stop_signal
        &HealthFlags::default(),
        super::RawRcFlags::default(),
        super::RawRunFlags::default(),
    );
    assert_eq!(
        code, 2,
        "--net=none on pure native must exit 2 (requires ns or vz)"
    );
}

/// A bad `--net` value is rejected at exit 2 end-to-end through `run`, before
/// any provisioning — proving the parse is wired into the handler.
#[test]
fn run_bad_net_value_exits_2() {
    let code = run(
        ".",
        &[],
        &[],
        &["true".to_string()],
        false,
        false,
        false,
        &[],
        false,
        &[],
        "native",
        None,
        "bridge", // invalid --net value
        false,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        None,
        None,
        None,
        None,
        &HealthFlags::default(),
        super::RawRcFlags::default(),
        super::RawRunFlags::default(),
    );
    assert_eq!(code, 2, "invalid --net value must exit 2");
}
