//! WP-NET3 — the switch-host re-exec marker dispatch (the keystone that lets the
//! per-network L2 switch BIRTH inside the real `lightr` binary). `attach` spawns
//! `current_exe()` with `[SWITCH_HOST_ARGV, <home>, <network_id>]`, which is NOT a
//! clap subcommand — `main` recognises it before `Cli::parse()` and routes it to
//! `run_switch_host`. We unit-test the recognition predicate (the dispatch itself
//! calls `process::exit`, so it can't run in-process). Unix-only — `vswitch` is
//! `#[cfg(unix)]`.

use lightr_run::vswitch::switch_host::SWITCH_HOST_ARGV;

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The exact shape `spawn_switch_host` produces is recognised + decomposed into
/// (home, network_id) — proving the marker routes to the switch-host body.
#[test]
fn marker_argv_is_recognised_and_decomposed() {
    let a = argv(&["lightr", SWITCH_HOST_ARGV, "/some/home", "mynet"]);
    let got = crate::switch_host_argv(&a);
    assert_eq!(got, Some(("/some/home", "mynet")));
}

/// Extra trailing args (forward-compat) still recognise; home/id stay [2]/[3].
#[test]
fn marker_argv_with_trailing_args_recognised() {
    let a = argv(&["lightr", SWITCH_HOST_ARGV, "/h", "net", "extra"]);
    assert_eq!(crate::switch_host_argv(&a), Some(("/h", "net")));
}

/// A normal CLI invocation is NOT mistaken for the marker (no false dispatch).
#[test]
fn ordinary_argv_is_not_a_marker() {
    assert!(crate::switch_host_argv(&argv(&["lightr", "run", "--", "true"])).is_none());
    assert!(crate::switch_host_argv(&argv(&["lightr", "ps"])).is_none());
    // Marker word but too few args ⇒ not dispatched (fail-closed).
    assert!(crate::switch_host_argv(&argv(&["lightr", SWITCH_HOST_ARGV, "/h"])).is_none());
}
