//! cfg(linux) pure-helper tests for the CNI chain derivation/result parsing.
//! These need NO privilege and NO filesystem writes (the netns syscalls + plugin
//! exec are exercised only on Linux CI / on-box — contract §5). The whole module
//! is gated `#[cfg(all(test, target_os = "linux"))]` so it never compiles on the
//! macOS or windows-cross lanes (8a).

use super::*;
use serde_json::json;

#[test]
fn derive_injects_version_and_name() {
    let plugin = json!({"type": "bridge", "bridge": "cni0"});
    let out = derive_plugin_config(&plugin, "1.0.0", "lightr", None, &[]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["cniVersion"], "1.0.0");
    assert_eq!(v["name"], "lightr");
    assert_eq!(v["bridge"], "cni0");
    assert!(v.get("prevResult").is_none());
}

#[test]
fn derive_threads_prev_result() {
    let plugin = json!({"type": "host-local"});
    let prev = json!({"ips": [{"address": "10.88.0.5/16"}]});
    let out = derive_plugin_config(&plugin, "1.0.0", "net", Some(&prev), &[]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["prevResult"]["ips"][0]["address"], "10.88.0.5/16");
}

#[test]
fn derive_portmap_runtime_config_and_omits_host_port_zero() {
    let plugin = json!({"type": "portmap", "capabilities": {"portMappings": true}});
    let pm_zero = PortMapping {
        protocol: Protocol::Tcp,
        container_port: 80,
        host_port: 0,
        host_ip: String::new(),
    };
    let pm_real = PortMapping {
        protocol: Protocol::Tcp,
        container_port: 443,
        host_port: 8443,
        host_ip: String::new(),
    };
    let out = derive_plugin_config(&plugin, "1.0.0", "net", None, &[pm_zero, pm_real]);
    let v: Value = serde_json::from_str(&out).unwrap();
    let pmaps = v["runtimeConfig"]["portMappings"].as_array().unwrap();
    assert_eq!(pmaps.len(), 1, "host_port=0 filtered");
    assert_eq!(pmaps[0]["hostPort"], 8443);
    assert_eq!(pmaps[0]["protocol"], "tcp");
}

#[test]
fn derive_no_cap_skips_runtime_config() {
    let plugin = json!({"type": "bridge"});
    let pm = PortMapping {
        protocol: Protocol::Udp,
        container_port: 53,
        host_port: 5353,
        host_ip: String::new(),
    };
    let out = derive_plugin_config(&plugin, "1.0.0", "net", None, &[pm]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("runtimeConfig").is_none());
}

#[test]
fn extract_ipv4_strips_prefix_and_skips_ipv6() {
    let r = json!({"ips": [{"address": "fd00::1/64"}, {"address": "10.1.2.3/24"}]});
    assert_eq!(extract_first_ipv4(&r), Some("10.1.2.3".to_string()));
    let empty = json!({"ips": []});
    assert_eq!(extract_first_ipv4(&empty), None);
}

#[test]
fn parse_cni_error_extracts_msg_then_details_then_raw() {
    let with_msg = r#"{"code":7,"msg":"already exists","details":"x"}"#;
    assert_eq!(parse_cni_error(with_msg), "already exists");
    let with_details = r#"{"code":1,"details":"boom"}"#;
    assert_eq!(parse_cni_error(with_details), "boom");
    assert_eq!(parse_cni_error("not json"), "not json");
}

#[test]
fn conflist_plugins_and_type() {
    let cl = json!({
        "cniVersion": "1.0.0", "name": "lightr",
        "plugins": [{"type": "bridge"}, {"type": "portmap"}]
    });
    assert_eq!(conflist_str(&cl, "cniVersion", "x"), "1.0.0");
    assert_eq!(conflist_str(&cl, "name", "x"), "lightr");
    let plugins = conflist_plugins(&cl).unwrap();
    assert_eq!(plugins.len(), 2);
    assert_eq!(plugin_type(&plugins[0]).unwrap(), "bridge");
    let missing = json!({"name": "x"});
    assert!(conflist_plugins(&missing).is_err());
}
