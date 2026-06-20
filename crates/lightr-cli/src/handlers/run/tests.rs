use super::{parse_mount, parse_publish, run, HealthFlags};

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
        &[],  // env_set (WP-RC-1)
        None, // env_file (WP-RC-1)
        &HealthFlags::default(),
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
        &[],  // env_set (WP-RC-1)
        None, // env_file (WP-RC-1)
        &HealthFlags::default(),
    );
    assert_eq!(code, 2, "-p on the engine path must exit 2 (Phase 2)");
}

// ── HealthFlags::build (WP-RC-4) ──────────────────────────────────────────

#[test]
fn health_flags_build_from_cmd() {
    // A --health-cmd with explicit timings lowers 1:1 to a Healthcheck.
    let flags = HealthFlags {
        cmd: Some("curl -fsS localhost/health".to_string()),
        interval: 15,
        timeout: 5,
        start_period: 10,
        retries: 4,
        no_healthcheck: false,
    };
    let hc = flags
        .build()
        .expect("a --health-cmd must build a Healthcheck");
    assert_eq!(hc.cmd, "curl -fsS localhost/health");
    assert_eq!(hc.interval_s, 15);
    assert_eq!(hc.timeout_s, 5);
    assert_eq!(hc.start_period_s, 10);
    assert_eq!(hc.retries, 4);
}

#[test]
fn health_flags_none_without_cmd() {
    // No --health-cmd ⇒ no healthcheck (the common case; behavior-preserving).
    let flags = HealthFlags {
        cmd: None,
        interval: 30,
        timeout: 30,
        start_period: 0,
        retries: 3,
        no_healthcheck: false,
    };
    assert!(flags.build().is_none(), "no --health-cmd ⇒ no healthcheck");
}

#[test]
fn health_flags_no_healthcheck_disables() {
    // --no-healthcheck wins even when --health-cmd is present (Docker
    // semantics: explicit disable beats a configured command).
    let flags = HealthFlags {
        cmd: Some("true".to_string()),
        interval: 30,
        timeout: 30,
        start_period: 0,
        retries: 3,
        no_healthcheck: true,
    };
    assert!(
        flags.build().is_none(),
        "--no-healthcheck must disable even with a --health-cmd"
    );
}

#[test]
fn health_flags_default_is_no_healthcheck() {
    // The Default (used by the docker-translation path + the no-flags run path)
    // builds no healthcheck — the behavior-preservation anchor.
    assert!(HealthFlags::default().build().is_none());
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

// ── WP-RC-1: `-e` is WIRED (no longer the WP-RUNFLAGS stub) ────────────────

/// A native run WITH `-e KEY=VAL` set actually RUNS (exit = the command's exit),
/// proving `-e`/`--env-file` were removed from the dispatch stub guard and flow
/// through to the keyed env_explicit channel. (The pre-WP-RC-1 guard returned
/// the WP-RUNFLAGS stub, exit 2, the instant `-e` was set.)
#[test]
fn dash_e_runs_not_stubbed() {
    let _env_guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    std::env::set_var("LIGHTR_HOME", tmp.path());

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir work");

    let code = run(
        work.to_str().unwrap(),
        &[],
        &[],
        &["true".to_string()],
        false, // json
        false, // explain
        false, // detach
        &[],   // publish
        &[],   // mounts
        "native",
        None,                     // rootfs
        false,                    // deep_memo
        None,                     // memory
        None,                     // cpus
        &[],                      // secrets
        &[],                      // configs
        &["FOO=bar".to_string()], // env_set (WP-RC-1) — must NOT be stubbed
        None,                     // env_file
        &HealthFlags::default(),
    );
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(
        code, 0,
        "a run with -e must execute the command (exit 0), not return the stub (exit 2)"
    );
}
