//! Runtime-parameter memo-key exclusion tests — split from memo_key.rs to keep
//! each file under the 400-LOC godfile cap (house convention). Every parameter
//! here is RUNTIME (like Docker's `-p`/`-w`/`-u`/`--restart`/`--stop-signal`):
//! two specs differing ONLY in it must key IDENTICALLY (no false cache miss).

use super::memo_key::{isolated_home, make_spec};
use crate::run::memo::build_key;
use crate::run::types::PortMap;
use std::fs;

#[test]
fn ports_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let mut spec_no_ports = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_no_ports.ports = vec![];

    let mut spec_with_ports = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_with_ports.ports = vec![
        PortMap {
            host: 8080,
            container: 80,
        },
        PortMap {
            host: 9090,
            container: 90,
        },
    ];

    let k1 = build_key(&spec_no_ports).expect("k1");
    let k2 = build_key(&spec_with_ports).expect("k2");
    assert_eq!(
        k1.0, k2.0,
        "ports must NOT affect the memo key (runtime-only, like -p in Docker)"
    );
}

#[test]
fn workdir_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let spec_no_wd = make_spec(cwd, vec!["/bin/echo", "x"]);

    let mut spec_with_wd = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_with_wd.workdir = Some("sub/wd".to_string());

    let k1 = build_key(&spec_no_wd).expect("k1");
    let k2 = build_key(&spec_with_wd).expect("k2");
    assert_eq!(
        k1.0, k2.0,
        "workdir must NOT affect the memo key (runtime-only, like -w in Docker)"
    );
}

#[test]
fn user_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let spec_no_user = make_spec(cwd, vec!["/bin/echo", "x"]);

    let mut spec_with_user = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_with_user.user = Some("1000:1000".to_string());

    let k1 = build_key(&spec_no_user).expect("k1");
    let k2 = build_key(&spec_with_user).expect("k2");
    assert_eq!(
        k1.0, k2.0,
        "user must NOT affect the memo key (runtime-only, like -u in Docker)"
    );
}

// WP-RC-RESTART: --restart is RUNTIME (Docker does not key on it).
#[test]
fn restart_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let spec_no_restart = make_spec(cwd, vec!["/bin/echo", "x"]);
    let mut spec_on_failure = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_on_failure.restart = Some("on-failure:3".to_string());

    let k0 = build_key(&spec_no_restart).expect("k0").0;
    let k1 = build_key(&spec_on_failure).expect("k1").0;
    assert_eq!(k0, k1, "restart must NOT affect the memo key");
}

// WP-RC-STOPSIGNAL: stop_signal is RUNTIME (Docker does not key on --stop-signal).
#[test]
fn stop_signal_excluded_from_key() {
    let (_home, _env_guard) = isolated_home();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    fs::write(cwd.join("f.txt"), b"data").unwrap();

    let spec_no_sig = make_spec(cwd, vec!["/bin/echo", "x"]);
    let mut spec_with_sig = make_spec(cwd, vec!["/bin/echo", "x"]);
    spec_with_sig.stop_signal = Some("SIGINT".to_string());

    let k0 = build_key(&spec_no_sig).expect("k0").0;
    let k1 = build_key(&spec_with_sig).expect("k1").0;
    assert_eq!(k0, k1, "stop_signal must NOT affect the memo key");
}
