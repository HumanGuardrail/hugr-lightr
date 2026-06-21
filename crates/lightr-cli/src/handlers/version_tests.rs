//! Tests for `lightr version`. Parallel-safe: no env mutation, no store.

use super::*;

#[test]
fn version_json_parses_and_has_no_fake_server() {
    let v = VersionJson {
        version: env!("CARGO_PKG_VERSION"),
        git_commit: env!("LIGHTR_GIT_SHA"),
        built: env!("LIGHTR_BUILD_DATE"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        daemonless: true,
        server: None,
    };
    let s = serde_json::to_string(&v).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();

    // Version is the real package version.
    assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
    // Daemonless is asserted true (principle #1).
    assert_eq!(parsed["daemonless"], true);
    // No fabricated server: the key is present and null.
    assert!(parsed.get("server").is_some(), "server key present");
    assert!(
        parsed["server"].is_null(),
        "server is null, never fabricated"
    );
    // OS/Arch are non-empty honest target facts.
    assert!(!parsed["os"].as_str().unwrap().is_empty());
    assert!(!parsed["arch"].as_str().unwrap().is_empty());
}

#[test]
fn version_run_exits_zero() {
    assert_eq!(run(false), 0);
    assert_eq!(run(true), 0);
}
