//! WP-B/WP-B2 publish-flag parse tests: port ranges (incl. width-mismatch
//! error), host-ip parse + CARRY (WP-B2 wired the host_ip into `PortMap`), and
//! `-P/--publish-all` EXPOSE synthesis. The runtime bind/forward that consumes
//! these PortMaps is exercised end-to-end in `lightr-run`'s `portforward` tests
//! (real TCP through a host-ip-bound forwarder).

use super::parse_publish;
use super::publish::{parse_publish_spec, synth_publish_all};
use lightr_run::PortMap;

// ---- single-port back-compat (parse_publish wrapper) ----

#[test]
fn single_port_maps_one_to_one() {
    assert_eq!(parse_publish("8080:80").unwrap(), PortMap::new(8080, 80));
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
            PortMap::new(8000, 9000),
            PortMap::new(8001, 9001),
            PortMap::new(8002, 9002),
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

// ---- host-ip binding (PARSE→spec + CARRY: WP-B2 threads host_ip into PortMap) ----

#[test]
fn host_ip_ipv4_parses_and_carries() {
    // 127.0.0.1:8080:80 ⇒ the host-ip is consumed AND carried onto the PortMap
    // (WP-B2 closed the runtime boundary). bind_ip() returns the loopback IP.
    let maps = parse_publish_spec("127.0.0.1:8080:80").unwrap();
    assert_eq!(maps.len(), 1);
    assert_eq!(maps[0].host, 8080);
    assert_eq!(maps[0].container, 80);
    assert_eq!(maps[0].host_ip, "127.0.0.1");
    assert_eq!(maps[0].bind_ip(), "127.0.0.1");
}

#[test]
fn host_ip_ipv4_with_range_carries_to_every_map() {
    let maps = parse_publish_spec("127.0.0.1:8000-8001:9000-9001").unwrap();
    assert_eq!(maps.len(), 2);
    assert_eq!(maps[0].host, 8000);
    assert_eq!(maps[1].container, 9001);
    // host_ip is carried onto EVERY expanded element of the range.
    assert!(maps.iter().all(|m| m.host_ip == "127.0.0.1"));
}

#[test]
fn host_ip_ipv6_bracketed_parses_and_carries() {
    let maps = parse_publish_spec("[::1]:8080:80").unwrap();
    assert_eq!(maps.len(), 1);
    assert_eq!(maps[0].host, 8080);
    assert_eq!(maps[0].container, 80);
    assert_eq!(maps[0].host_ip, "::1");
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
fn no_host_ip_defaults_to_all_interfaces() {
    // No host-ip prefix ⇒ empty host_ip, which bind_ip() maps to 0.0.0.0.
    let maps = parse_publish_spec("8080:80").unwrap();
    assert_eq!(maps[0].host_ip, "");
    assert_eq!(maps[0].bind_ip(), "0.0.0.0");
}

// ---- -P / --publish-all (EXPOSE synthesis) ----

#[test]
fn publish_all_one_mapping_per_expose() {
    let expose = vec!["80/tcp".to_string(), "443/tcp".to_string()];
    let maps = synth_publish_all(&expose);
    assert_eq!(maps, vec![PortMap::new(80, 80), PortMap::new(443, 443)]);
}

#[test]
fn publish_all_skips_udp_phase1() {
    let expose = vec!["80/tcp".to_string(), "53/udp".to_string()];
    let maps = synth_publish_all(&expose);
    assert_eq!(maps, vec![PortMap::new(80, 80)]);
}

#[test]
fn publish_all_bare_port_no_proto() {
    // Some EXPOSE lists omit the proto; bare "8080" is treated as tcp.
    let maps = synth_publish_all(&["8080".to_string()]);
    assert_eq!(maps, vec![PortMap::new(8080, 8080)]);
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

/// WP-B2 `-P` data path, end-to-end through a real image-config sidecar: an
/// `ImageConfig` carrying EXPOSE is written to a layer dir, loaded back, and
/// lowered to PortMaps — exactly the `expose_port_maps` chain the `-P` run-path
/// branch runs after hydrating the rootfs (minus the store hydrate). Proves `-P`
/// auto-publishes the image's EXPOSE list, each on the default interface.
#[test]
fn publish_all_loads_expose_from_image_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = lightr_build::ImageConfig {
        expose: vec![
            "8080/tcp".to_string(),
            "9090".to_string(),
            "53/udp".to_string(),
        ],
        ..Default::default()
    };
    cfg.save(dir.path()).expect("save image config sidecar");

    // The exact two steps `expose_port_maps` performs post-hydrate.
    let loaded = lightr_build::ImageConfig::load(dir.path());
    let maps = synth_publish_all(&loaded.expose);

    // tcp 8080 + bare 9090 published; udp 53 skipped (Phase-1 tcp-only). Each on
    // the default interface (empty host_ip ⇒ 0.0.0.0).
    assert_eq!(
        maps,
        vec![PortMap::new(8080, 8080), PortMap::new(9090, 9090)]
    );
    assert!(maps.iter().all(|m| m.bind_ip() == "0.0.0.0"));
}
