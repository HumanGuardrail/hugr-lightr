//! WP-B publish-flag parse tests: port ranges (incl. width-mismatch error),
//! host-ip parse, and `-P/--publish-all` EXPOSE synthesis. These exercise the
//! PARSE→spec layer only — the runtime bind/forward (host-ip carry, ephemeral
//! host-port assignment) lives in `lightr-run` and is out of this WP's scope
//! (see the RUNTIME BOUNDARY notes on `split_host_ip` / `synth_publish_all`).

use super::parse_publish;
use super::publish::{parse_publish_spec, synth_publish_all};
use lightr_run::PortMap;

// ---- single-port back-compat (parse_publish wrapper) ----

#[test]
fn single_port_maps_one_to_one() {
    assert_eq!(
        parse_publish("8080:80").unwrap(),
        PortMap {
            host: 8080,
            container: 80
        }
    );
}

#[test]
fn single_port_tcp_suffix_ok_udp_rejected() {
    assert!(parse_publish("8080:80/tcp").is_ok());
    assert_eq!(parse_publish("8080:80/udp").unwrap_err(), 2);
}

#[test]
fn single_wrapper_rejects_a_range() {
    // parse_publish promises exactly one mapping; a range must be rejected.
    assert_eq!(parse_publish("8000-8002:9000-9002").unwrap_err(), 2);
}

// ---- range expansion ----

#[test]
fn range_expands_element_wise() {
    let maps = parse_publish_spec("8000-8002:9000-9002").unwrap();
    assert_eq!(
        maps,
        vec![
            PortMap {
                host: 8000,
                container: 9000
            },
            PortMap {
                host: 8001,
                container: 9001
            },
            PortMap {
                host: 8002,
                container: 9002
            },
        ]
    );
}

#[test]
fn range_with_proto_suffix_expands() {
    let maps = parse_publish_spec("8000-8001:8000-8001/tcp").unwrap();
    assert_eq!(maps.len(), 2);
    assert_eq!(maps[0].host, 8000);
    assert_eq!(maps[1].container, 8001);
}

#[test]
fn range_width_mismatch_is_honest_error() {
    // host width 3, container width 1 ⇒ reject (no silent truncation).
    assert_eq!(parse_publish_spec("8000-8002:80").unwrap_err(), 2);
    // host width 1, container width 3 ⇒ reject.
    assert_eq!(parse_publish_spec("80:8000-8002").unwrap_err(), 2);
}

#[test]
fn range_low_greater_than_high_rejected() {
    assert_eq!(parse_publish_spec("9000-8000:9000-8000").unwrap_err(), 2);
}

#[test]
fn range_zero_port_rejected() {
    assert_eq!(parse_publish_spec("0-2:0-2").unwrap_err(), 2);
}

// ---- host-ip binding (PARSE→spec; runtime carry is gated) ----

#[test]
fn host_ip_ipv4_parses_and_yields_mapping() {
    // 127.0.0.1:8080:80 ⇒ the host-ip is consumed; the HOST:CONTAINER survives.
    let maps = parse_publish_spec("127.0.0.1:8080:80").unwrap();
    assert_eq!(
        maps,
        vec![PortMap {
            host: 8080,
            container: 80
        }]
    );
}

#[test]
fn host_ip_ipv4_with_range() {
    let maps = parse_publish_spec("127.0.0.1:8000-8001:9000-9001").unwrap();
    assert_eq!(maps.len(), 2);
    assert_eq!(maps[0].host, 8000);
    assert_eq!(maps[1].container, 9001);
}

#[test]
fn host_ip_ipv6_bracketed_parses() {
    let maps = parse_publish_spec("[::1]:8080:80").unwrap();
    assert_eq!(
        maps,
        vec![PortMap {
            host: 8080,
            container: 80
        }]
    );
}

#[test]
fn host_ip_invalid_ipv4_rejected() {
    assert_eq!(parse_publish_spec("999.1.1.1:8080:80").unwrap_err(), 2);
}

#[test]
fn host_ip_unclosed_bracket_rejected() {
    assert_eq!(parse_publish_spec("[::1:8080:80").unwrap_err(), 2);
}

#[test]
fn no_host_ip_defaults_silently() {
    // No host-ip prefix ⇒ still parses (default 0.0.0.0, not carried yet).
    assert!(parse_publish_spec("8080:80").is_ok());
}

// ---- -P / --publish-all (EXPOSE synthesis) ----

#[test]
fn publish_all_one_mapping_per_expose() {
    let expose = vec!["80/tcp".to_string(), "443/tcp".to_string()];
    let maps = synth_publish_all(&expose);
    assert_eq!(
        maps,
        vec![
            PortMap {
                host: 80,
                container: 80
            },
            PortMap {
                host: 443,
                container: 443
            },
        ]
    );
}

#[test]
fn publish_all_skips_udp_phase1() {
    let expose = vec!["80/tcp".to_string(), "53/udp".to_string()];
    let maps = synth_publish_all(&expose);
    assert_eq!(
        maps,
        vec![PortMap {
            host: 80,
            container: 80
        }]
    );
}

#[test]
fn publish_all_bare_port_no_proto() {
    // Some EXPOSE lists omit the proto; bare "8080" is treated as tcp.
    let maps = synth_publish_all(&["8080".to_string()]);
    assert_eq!(
        maps,
        vec![PortMap {
            host: 8080,
            container: 8080
        }]
    );
}

#[test]
fn publish_all_empty_expose_is_empty() {
    assert!(synth_publish_all(&[]).is_empty());
}

#[test]
fn publish_all_skips_malformed_entries() {
    // Image-provided list ⇒ fail-soft: skip junk, keep the good one.
    let expose = vec!["notaport".to_string(), "8080/tcp".to_string()];
    let maps = synth_publish_all(&expose);
    assert_eq!(maps.len(), 1);
    assert_eq!(maps[0].host, 8080);
}
