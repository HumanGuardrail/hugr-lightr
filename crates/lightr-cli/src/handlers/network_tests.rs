//! Parallel-safe unit tests for the `network` verbs.
//!
//! Each test injects its OWN tempdir as `home` (house convention — see
//! `network::registry`); NOTHING mutates the global env, so the suite is safe
//! under `cargo test` multi-threaded.

use super::*;
use lightr_run::network::NetworkRegistry;
use tempfile::TempDir;

fn home() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

// ── ls ────────────────────────────────────────────────────────────────────────

#[test]
fn ls_shows_predefined_on_empty_home() {
    let h = home();
    let rows = ls_rows(h.path()).expect("ls_rows");
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["bridge", "host", "none"]);
}

#[test]
fn ls_lists_user_networks_after_predefined() {
    let h = home();
    NetworkRegistry::create(h.path(), &"web".to_string()).expect("create");
    let rows = ls_rows(h.path()).expect("ls_rows");
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["bridge", "host", "none", "web"]);
    // user network carries the bridge driver + local scope
    let web = rows.iter().find(|r| r.name == "web").unwrap();
    assert_eq!(web.driver, "bridge");
    assert_eq!(web.scope, "local");
}

#[test]
fn ls_renders_clean_in_both_modes() {
    let h = home();
    NetworkRegistry::create(h.path(), &"db".to_string()).expect("create");
    ls(h.path(), false).expect("text ls ok");
    ls(h.path(), true).expect("json ls ok");
}

// ── create ──────────────────────────────────────────────────────────────────

#[test]
fn create_makes_a_user_network() {
    let h = home();
    create(h.path(), "web").expect("create");
    assert!(NetworkRegistry::open(h.path(), &"web".to_string()).is_ok());
}

#[test]
fn create_errors_when_already_exists() {
    let h = home();
    create(h.path(), "web").expect("first create");
    let err = create(h.path(), "web").unwrap_err();
    assert!(matches!(err, LightrError::InvalidRef(_)), "got {err:?}");
}

#[test]
fn create_rejects_predefined_names() {
    let h = home();
    for n in ["bridge", "host", "none"] {
        let err = create(h.path(), n).unwrap_err();
        assert!(matches!(err, LightrError::InvalidRef(_)), "{n}: {err:?}");
    }
}

#[test]
fn create_rejects_invalid_names() {
    let h = home();
    let err = create(h.path(), "bad name!").unwrap_err();
    assert!(matches!(err, LightrError::InvalidRef(_)), "got {err:?}");
}

// ── rm ────────────────────────────────────────────────────────────────────────

#[test]
fn rm_removes_an_empty_user_network() {
    let h = home();
    create(h.path(), "web").expect("create");
    rm(h.path(), &["web".to_string()]).expect("rm");
    assert!(NetworkRegistry::open(h.path(), &"web".to_string()).is_err());
}

#[test]
fn rm_errors_on_missing_network() {
    let h = home();
    let err = rm(h.path(), &["nope".to_string()]).unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)), "got {err:?}");
}

#[test]
fn rm_rejects_predefined_networks() {
    let h = home();
    for n in ["bridge", "host", "none"] {
        let err = rm(h.path(), &[n.to_string()]).unwrap_err();
        assert!(matches!(err, LightrError::InvalidRef(_)), "{n}: {err:?}");
    }
}

#[test]
fn rm_errors_when_network_in_use() {
    let h = home();
    let reg = NetworkRegistry::create(h.path(), &"busy".to_string()).expect("create");
    reg.join("ctr1", &[], &[]).expect("join");
    let err = rm(h.path(), &["busy".to_string()]).unwrap_err();
    // in-use is a runtime error (exit-1), not a usage error
    assert!(matches!(err, LightrError::Io(_)), "got {err:?}");
    // the network is NOT removed
    assert!(NetworkRegistry::open(h.path(), &"busy".to_string()).is_ok());
}

#[test]
fn rm_errors_on_empty_target_list() {
    let h = home();
    let err = rm(h.path(), &[]).unwrap_err();
    assert!(matches!(err, LightrError::InvalidRef(_)), "got {err:?}");
}

// ── inspect ───────────────────────────────────────────────────────────────────

#[test]
fn inspect_prints_subnet_and_members() {
    let h = home();
    let reg = NetworkRegistry::create(h.path(), &"web".to_string()).expect("create");
    let m = reg
        .join("ctr1", &["alias1".to_string()], &[])
        .expect("join");
    // Build the same JSON the handler emits and assert its shape.
    let subnet = reg.subnet();
    let members = reg.members().expect("members");
    let out = InspectJson {
        name: "web".to_string(),
        subnet: subnet_cidr(&subnet),
        gateway: subnet.gateway.to_string(),
        members: members.iter().map(member_json).collect(),
    };
    let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&out).unwrap()).unwrap();
    assert_eq!(v["name"], "web");
    assert!(v["subnet"].as_str().unwrap().contains('/'));
    assert_eq!(v["members"][0]["name"], "ctr1");
    assert_eq!(v["members"][0]["ipv4"], m.ip.to_string());
    assert_eq!(v["members"][0]["aliases"][0], "alias1");
    // the handler path itself runs clean
    inspect(h.path(), "web").expect("inspect ok");
}

#[test]
fn inspect_errors_on_missing_network() {
    let h = home();
    let err = inspect(h.path(), "nope").unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)), "got {err:?}");
}

#[test]
fn inspect_rejects_predefined_networks() {
    let h = home();
    let err = inspect(h.path(), "bridge").unwrap_err();
    assert!(matches!(err, LightrError::InvalidRef(_)), "got {err:?}");
}

// ── connect / disconnect ──────────────────────────────────────────────────────

#[test]
fn connect_is_honest_exit_2() {
    let code = run(NetworkCmd::Connect {
        network: "web".to_string(),
        container: "ctr1".to_string(),
    });
    assert_eq!(code, 2);
}

#[test]
fn disconnect_is_honest_exit_2() {
    let code = run(NetworkCmd::Disconnect {
        network: "web".to_string(),
        container: "ctr1".to_string(),
    });
    assert_eq!(code, 2);
}

// ── helpers ───────────────────────────────────────────────────────────────────

#[test]
fn mac_hex_formats_six_octets() {
    let m = MacAddr([0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]);
    assert_eq!(mac_hex(&m), "de:ad:be:ef:00:01");
}
