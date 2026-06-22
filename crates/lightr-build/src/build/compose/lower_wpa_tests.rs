//! WP-A: per-key lowering tests for the compose-lowering batch —
//! entrypoint / extra_hosts / stop_grace_period / stop_signal / hostname /
//! stdin_open. Each test lowers a focused compose YAML through the dispatcher
//! and asserts the key took effect on the runtime `Service` (or, for the two
//! run-side-gap keys, proves the lowering is a clean no-op).
use super::*;

/// Lower one-service compose YAML and return the single lowered `Service`.
fn lower_one(yaml: &str) -> Service {
    let spec: ComposeSpec = serde_yaml::from_str(yaml).unwrap();
    let mut c = lower(spec).unwrap();
    c.services.remove(0)
}

// --- entrypoint: override the image ENTRYPOINT (→ RunSpec.entrypoint) ---

#[test]
fn entrypoint_exec_form_is_argv_as_is() {
    let s =
        lower_one("services:\n  web:\n    image: x\n    entrypoint: [\"/entry\", \"--flag\"]\n");
    assert_eq!(
        s.entrypoint,
        Some(vec!["/entry".to_string(), "--flag".to_string()])
    );
}

#[test]
fn entrypoint_shell_string_is_sh_c_wrapped() {
    // Mirrors `lower_command`: a bare string becomes `/bin/sh -c <str>`.
    let s = lower_one("services:\n  web:\n    image: x\n    entrypoint: \"run me\"\n");
    assert_eq!(
        s.entrypoint,
        Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "run me".to_string()
        ])
    );
}

#[test]
fn entrypoint_absent_is_none() {
    let s = lower_one("services:\n  web:\n    image: x\n");
    assert_eq!(s.entrypoint, None);
}

// --- extra_hosts: inject /etc/hosts entries (→ RunSpec.add_host) ---

#[test]
fn extra_hosts_list_form_is_verbatim() {
    let s = lower_one(
        "services:\n  web:\n    image: x\n    extra_hosts:\n      - \"a:1.2.3.4\"\n      - \"b:5.6.7.8\"\n",
    );
    assert_eq!(s.extra_hosts, vec!["a:1.2.3.4", "b:5.6.7.8"]);
}

#[test]
fn extra_hosts_map_form_joins_host_colon_ip() {
    let s =
        lower_one("services:\n  web:\n    image: x\n    extra_hosts:\n      somehost: 9.9.9.9\n");
    assert_eq!(s.extra_hosts, vec!["somehost:9.9.9.9"]);
}

#[test]
fn extra_hosts_absent_is_empty() {
    let s = lower_one("services:\n  web:\n    image: x\n");
    assert!(s.extra_hosts.is_empty());
}

// --- stop_signal: graceful-stop signal (→ RunSpec.stop_signal) ---

#[test]
fn stop_signal_is_transcribed_verbatim() {
    let s = lower_one("services:\n  web:\n    image: x\n    stop_signal: SIGINT\n");
    assert_eq!(s.stop_signal.as_deref(), Some("SIGINT"));
}

// --- hostname: container hostname (→ RunSpec.hostname) ---

#[test]
fn hostname_is_transcribed_verbatim() {
    let s = lower_one("services:\n  web:\n    image: x\n    hostname: web-1\n");
    assert_eq!(s.hostname.as_deref(), Some("web-1"));
}

// --- stop_grace_period: LOWERED-TO-NOOP (run side lacks a stop-grace slot) ---

#[test]
fn stop_grace_period_lowers_clean_noop() {
    // The field parses and lowers without error; it touches no runtime slot
    // (no stop-grace field exists on Service / RunSpec).
    let s = lower_one("services:\n  web:\n    image: x\n    stop_grace_period: 30s\n");
    let bare = lower_one("services:\n  web:\n    image: x\n");
    // Behavior-preserving: the lowered Service is byte-identical to the bare one
    // on the fields WP-A touches (no stop-grace slot to differ on).
    assert_eq!(s.entrypoint, bare.entrypoint);
    assert_eq!(s.extra_hosts, bare.extra_hosts);
    assert_eq!(s.stop_signal, bare.stop_signal);
    assert_eq!(s.hostname, bare.hostname);
}

// --- stdin_open: LOWERED-TO-NOOP (run side lacks an interactive/stdin slot) ---

#[test]
fn stdin_open_lowers_clean_noop() {
    let s = lower_one("services:\n  web:\n    image: x\n    stdin_open: true\n");
    let bare = lower_one("services:\n  web:\n    image: x\n");
    assert_eq!(s.entrypoint, bare.entrypoint);
    assert_eq!(s.extra_hosts, bare.extra_hosts);
    assert_eq!(s.stop_signal, bare.stop_signal);
    assert_eq!(s.hostname, bare.hostname);
}
