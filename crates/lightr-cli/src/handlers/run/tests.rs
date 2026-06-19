use super::{parse_mount, parse_publish, run};

// ── parse_publish ───────────────────────────────────────────────────────

#[test]
fn publish_parses_host_container() {
    let p = parse_publish("8080:80").expect("should parse");
    assert_eq!(p.host, 8080);
    assert_eq!(p.container, 80);
}

#[test]
fn publish_accepts_explicit_tcp() {
    let p = parse_publish("39000:39001/tcp").expect("should parse");
    assert_eq!(p.host, 39000);
    assert_eq!(p.container, 39001);
}

#[test]
fn publish_rejects_udp_as_phase2() {
    let r = parse_publish("8080:80/udp");
    assert!(r.is_err());
    assert_eq!(r.err().unwrap(), 2);
}

#[test]
fn publish_rejects_missing_colon() {
    assert_eq!(parse_publish("8080").err().unwrap(), 2);
}

#[test]
fn publish_rejects_zero_port() {
    assert_eq!(parse_publish("0:80").err().unwrap(), 2);
    assert_eq!(parse_publish("80:0").err().unwrap(), 2);
}

#[test]
fn publish_rejects_out_of_range_and_nonnumeric() {
    // 70000 > u16::MAX ⇒ parse fails ⇒ Err(2).
    assert_eq!(parse_publish("70000:80").err().unwrap(), 2);
    assert_eq!(parse_publish("8080:abc").err().unwrap(), 2);
}

// ── policy guards (return 2 BEFORE any store/engine work) ─────────────────

#[test]
fn publish_without_detach_exits_2() {
    // -p given, detach=false ⇒ exit 2 (guard 1), before Store::open.
    let code = run(
        ".",
        &[],
        &[],
        &["true".to_string()],
        false, // json
        false, // explain
        false, // detach  ← NOT detached
        &["39000:39001".to_string()],
        &[],
        "native",
        None,
        false,
        None,
        None,
        &[],
        &[],
        None,
        30,
        3,
    );
    assert_eq!(code, 2, "-p without -d must exit 2");
}

#[test]
fn publish_on_engine_path_exits_2() {
    // -p + -d but engine=vz ⇒ exit 2 (guard 2), before the engine early
    // return / any store work.
    let code = run(
        ".",
        &[],
        &[],
        &["true".to_string()],
        false,
        false,
        true, // detach
        &["39000:39001".to_string()],
        &[],
        "vz", // engine path ⇒ Phase 2
        None,
        false,
        None,
        None,
        &[],
        &[],
        None,
        30,
        3,
    );
    assert_eq!(code, 2, "-p on the engine path must exit 2 (Phase 2)");
}

// ── parse_mount (existing) ────────────────────────────────────────────────

#[test]
fn mount_parse_splits_on_first_colon() {
    let m = parse_mount("myref:some/target").expect("should parse");
    assert_eq!(m.ref_name, "myref");
    assert_eq!(m.target, "some/target");
}

#[test]
fn mount_parse_splits_on_first_colon_extra_colons() {
    // "ref:sub:extra" → ref_name="ref", target="sub:extra" (split on FIRST colon)
    let m = parse_mount("ref:sub:extra").expect("should parse");
    assert_eq!(m.ref_name, "ref");
    assert_eq!(m.target, "sub:extra");
}

#[test]
fn mount_rejects_absolute_target() {
    let result = parse_mount("ref:/abs/path");
    assert!(result.is_err());
    assert_eq!(result.err().unwrap(), 2);
}

#[test]
fn mount_rejects_invalid_ref_name() {
    // Uppercase ref name is invalid
    let result = parse_mount("INVALID:target");
    assert!(result.is_err());
    assert_eq!(result.err().unwrap(), 2);
}

#[test]
fn mount_rejects_missing_colon() {
    let result = parse_mount("nocoton");
    assert!(result.is_err());
    assert_eq!(result.err().unwrap(), 2);
}

#[test]
fn mount_accepts_relative_target() {
    let m = parse_mount("valid-ref:sub/dir").expect("should parse");
    assert_eq!(m.ref_name, "valid-ref");
    assert_eq!(m.target, "sub/dir");
}
