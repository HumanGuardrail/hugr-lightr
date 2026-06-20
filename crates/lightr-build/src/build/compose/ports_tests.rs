//! Tests for compose/ports.rs — the full compose `ports` grammar.
//!
//! Parallel-safe: pure parsing, no process-global state, no tempdirs.
use super::*;
use crate::build::compose::parse::parse_compose;
use crate::build::compose::spec::{ComposeSpec, PortLong, PortSpec};

/// Build the `PortSpec` list from a service's inline `ports:` YAML.
fn specs(yaml: &str) -> Vec<PortSpec> {
    let full = format!("services:\n  svc:\n    image: i\n    ports:\n{yaml}");
    let spec: ComposeSpec = serde_yaml::from_str(&full).unwrap();
    let svc = spec.services.into_iter().next().unwrap().1;
    svc.ports
}

fn pp(host_ip: &str, published: Option<u16>, target: u16, proto: &str) -> ParsedPort {
    ParsedPort {
        host_ip: host_ip.to_string(),
        published,
        target,
        proto: proto.to_string(),
    }
}

// ---- short: host:container ------------------------------------------------

#[test]
fn short_host_container() {
    let got = parse_ports(&specs("      - \"8080:80\"\n")).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", Some(8080), 80, "tcp")]);
}

#[test]
fn short_bare_number_yaml_int() {
    // `- 8080` parses as a YAML number, container-only.
    let got = parse_ports(&specs("      - 8080\n")).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", None, 8080, "tcp")]);
}

#[test]
fn short_container_only_string() {
    let got = parse_ports(&specs("      - \"80\"\n")).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", None, 80, "tcp")]);
}

// ---- short: host_ip -------------------------------------------------------

#[test]
fn short_host_ip() {
    let got = parse_ports(&specs("      - \"127.0.0.1:8080:80\"\n")).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", Some(8080), 80, "tcp")]);
}

#[test]
fn short_host_ip_non_loopback() {
    let got = parse_ports(&specs("      - \"0.0.0.0:9000:90\"\n")).unwrap();
    assert_eq!(got, vec![pp("0.0.0.0", Some(9000), 90, "tcp")]);
}

#[test]
fn short_host_ip_container_only() {
    // host_ip + container, no host port → host auto-assigned.
    let got = parse_ports(&specs("      - \"127.0.0.1::80\"\n")).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", None, 80, "tcp")]);
}

// ---- short: proto ---------------------------------------------------------

#[test]
fn short_udp_proto() {
    let got = parse_ports(&specs("      - \"8080:80/udp\"\n")).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", Some(8080), 80, "udp")]);
}

#[test]
fn short_proto_uppercase_normalized() {
    let got = parse_ports(&specs("      - \"53:53/UDP\"\n")).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", Some(53), 53, "udp")]);
}

#[test]
fn short_host_ip_range_proto_combined() {
    let got = parse_ports(&specs("      - \"127.0.0.1:3000-3001:3000-3001/udp\"\n")).unwrap();
    assert_eq!(
        got,
        vec![
            pp("127.0.0.1", Some(3000), 3000, "udp"),
            pp("127.0.0.1", Some(3001), 3001, "udp"),
        ]
    );
}

// ---- short: ranges --------------------------------------------------------

#[test]
fn short_range_expands() {
    let got = parse_ports(&specs("      - \"3000-3002:3000-3002\"\n")).unwrap();
    assert_eq!(
        got,
        vec![
            pp("127.0.0.1", Some(3000), 3000, "tcp"),
            pp("127.0.0.1", Some(3001), 3001, "tcp"),
            pp("127.0.0.1", Some(3002), 3002, "tcp"),
        ]
    );
}

#[test]
fn short_range_remapped_offset() {
    let got = parse_ports(&specs("      - \"8000-8001:80-81\"\n")).unwrap();
    assert_eq!(
        got,
        vec![
            pp("127.0.0.1", Some(8000), 80, "tcp"),
            pp("127.0.0.1", Some(8001), 81, "tcp"),
        ]
    );
}

#[test]
fn short_container_only_range() {
    let got = parse_ports(&specs("      - \"3000-3001\"\n")).unwrap();
    assert_eq!(
        got,
        vec![
            pp("127.0.0.1", None, 3000, "tcp"),
            pp("127.0.0.1", None, 3001, "tcp"),
        ]
    );
}

#[test]
fn short_range_length_mismatch_errors() {
    let err = parse_ports(&specs("      - \"3000-3005:3000-3001\"\n")).unwrap_err();
    assert!(format!("{err:?}").contains("range len"), "{err:?}");
}

#[test]
fn short_inverted_range_errors() {
    assert!(parse_ports(&specs("      - \"3005-3000:3005-3000\"\n")).is_err());
}

// ---- long form ------------------------------------------------------------

#[test]
fn long_full() {
    let yaml = "      - target: 80\n        published: 8080\n        protocol: tcp\n        host_ip: 127.0.0.1\n        mode: host\n";
    let got = parse_ports(&specs(yaml)).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", Some(8080), 80, "tcp")]);
}

#[test]
fn long_defaults_proto_and_host_ip() {
    // Only target + published given → tcp + loopback defaults.
    let yaml = "      - target: 80\n        published: 8080\n";
    let got = parse_ports(&specs(yaml)).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", Some(8080), 80, "tcp")]);
}

#[test]
fn long_udp_non_loopback() {
    let yaml = "      - target: 53\n        published: 5353\n        protocol: udp\n        host_ip: 0.0.0.0\n";
    let got = parse_ports(&specs(yaml)).unwrap();
    assert_eq!(got, vec![pp("0.0.0.0", Some(5353), 53, "udp")]);
}

#[test]
fn long_no_published_auto_assign() {
    let yaml = "      - target: 80\n";
    let got = parse_ports(&specs(yaml)).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", None, 80, "tcp")]);
}

#[test]
fn long_missing_target_errors() {
    let yaml = "      - published: 8080\n        protocol: tcp\n";
    assert!(parse_ports(&specs(yaml)).is_err());
}

#[test]
fn mixed_short_and_long_in_one_list() {
    let yaml = "      - \"8080:80\"\n      - target: 443\n        published: 8443\n";
    let got = parse_ports(&specs(yaml)).unwrap();
    assert_eq!(
        got,
        vec![
            pp("127.0.0.1", Some(8080), 80, "tcp"),
            pp("127.0.0.1", Some(8443), 443, "tcp"),
        ]
    );
}

// ---- malformed (fail-closed) ----------------------------------------------

#[test]
fn malformed_non_numeric_errors() {
    assert!(parse_ports(&specs("      - \"abc:80\"\n")).is_err());
    assert!(parse_ports(&specs("      - \"8080:xyz\"\n")).is_err());
}

#[test]
fn malformed_overflow_port_errors() {
    // 70000 > u16::MAX → fail-closed, not silently truncated.
    assert!(parse_ports(&specs("      - \"70000:80\"\n")).is_err());
}

#[test]
fn malformed_empty_proto_errors() {
    assert!(parse_ports(&specs("      - \"8080:80/\"\n")).is_err());
}

#[test]
fn malformed_empty_string_errors() {
    assert!(parse_ports(&specs("      - \"\"\n")).is_err());
}

// ---- behavior preservation of plain "H:C" via the full pipeline -----------

#[test]
fn behavior_preserved_plain_host_container_lowers_identically() {
    // The lowered `Service.ports` (Vec<(u16,u16)>) for a plain short file must
    // be byte-identical to the legacy parser's output.
    let yaml = "services:\n  web:\n    image: myimage\n    ports:\n      - \"8080:80\"\n  db:\n    image: dbimage\n    ports:\n      - \"5432:5432\"\n";
    let c = parse_compose(yaml).unwrap();
    assert_eq!(c.services[0].ports, vec![(8080u16, 80u16)]);
    assert_eq!(c.services[1].ports, vec![(5432u16, 5432u16)]);
}

#[test]
fn lowering_drops_proto_hostip_and_auto_assign_at_model_boundary() {
    // `Service.ports` is TCP-only `(host, container)`. A container-only entry
    // (auto-assign) and proto/host_ip are dropped at the model boundary,
    // preserving the legacy parser which ignored `:`-less short entries.
    let yaml = "services:\n  s:\n    image: i\n    ports:\n      - \"127.0.0.1:9000:90/udp\"\n      - \"80\"\n";
    let c = parse_compose(yaml).unwrap();
    // Only the published mapping survives, proto/host_ip dropped to the pair.
    assert_eq!(c.services[0].ports, vec![(9000u16, 90u16)]);
}

#[test]
fn lowering_range_expands_into_model_pairs() {
    let yaml = "services:\n  s:\n    image: i\n    ports:\n      - \"3000-3002:3000-3002\"\n";
    let c = parse_compose(yaml).unwrap();
    assert_eq!(
        c.services[0].ports,
        vec![(3000, 3000), (3001, 3001), (3002, 3002)]
    );
}

#[test]
fn lowering_malformed_port_fails_compose() {
    let yaml = "services:\n  s:\n    image: i\n    ports:\n      - \"oops:80\"\n";
    assert!(parse_compose(yaml).is_err(), "malformed port fails-closed");
}

// Keep `PortLong` directly exercised so the struct path is covered even if the
// untagged enum routing changes.
#[test]
fn port_long_struct_direct() {
    let m = PortLong {
        target: Some(80),
        published: Some(8080),
        protocol: Some("TCP".to_string()),
        host_ip: Some("  ".to_string()), // whitespace → default loopback
        mode: None,
    };
    let got = parse_ports(&[PortSpec::Long(m)]).unwrap();
    assert_eq!(got, vec![pp("127.0.0.1", Some(8080), 80, "tcp")]);
}
