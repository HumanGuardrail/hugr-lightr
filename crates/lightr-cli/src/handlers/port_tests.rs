//! Tests for the `port` handler — split out of `port.rs` (house convention:
//! `#[cfg(test)] #[path] mod tests;`) to keep each .rs total under the 400-line
//! godfile cap. Parallel-safe: every test uses its own tempdir and touches no
//! process-global state (no env mutation).

use super::{parse_port_arg, read_mappings};
use std::sync::atomic::{AtomicU64, Ordering};

// Unique tempdir per test (atomic counter + nanos) — parallel-safe, no shared
// path, no env mutation.
static SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> std::path::PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lightr-port-test-{n}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_spec(dir: &std::path::Path, json: &str) {
    std::fs::write(dir.join("spec.json"), json).unwrap();
}

#[test]
fn parse_port_arg_bare_and_proto() {
    assert_eq!(parse_port_arg("8080"), Some((8080, "tcp".to_string())));
    assert_eq!(parse_port_arg("53/udp"), Some((53, "udp".to_string())));
    assert_eq!(parse_port_arg("443/TCP"), Some((443, "tcp".to_string())));
    assert_eq!(parse_port_arg(" 80 "), Some((80, "tcp".to_string())));
    assert_eq!(parse_port_arg("nope"), None);
    assert_eq!(parse_port_arg(""), None);
    assert_eq!(parse_port_arg("99999"), None); // out of u16 range
}

#[test]
fn read_mappings_legacy_ports_channel() {
    let dir = unique_dir();
    write_spec(
        &dir,
        r#"{"cwd":"/","command":["x"],"env_keys":[],"mounts":[],"detached":true,"created_at_unix":0,"ports":[[8080,80],[5432,5432]]}"#,
    );
    let m = read_mappings(&dir);
    assert_eq!(m.len(), 2);
    assert!(m
        .iter()
        .any(|x| x.container == 80 && x.host == 8080 && x.proto == "tcp"));
    assert!(m
        .iter()
        .any(|x| x.container == 5432 && x.host == 5432 && x.proto == "tcp"));
}

#[test]
fn read_mappings_ports2_proto_channel() {
    let dir = unique_dir();
    write_spec(
        &dir,
        r#"{"cwd":"/","command":["x"],"env_keys":[],"mounts":[],"detached":true,"created_at_unix":0,"ports2":[{"host":9000,"container":90,"proto":"udp"}]}"#,
    );
    let m = read_mappings(&dir);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].container, 90);
    assert_eq!(m[0].host, 9000);
    assert_eq!(m[0].proto, "udp");
}

#[test]
fn read_mappings_ports2_default_proto_is_tcp() {
    let dir = unique_dir();
    write_spec(
        &dir,
        r#"{"cwd":"/","command":["x"],"env_keys":[],"mounts":[],"detached":true,"created_at_unix":0,"ports2":[{"host":9000,"container":90}]}"#,
    );
    let m = read_mappings(&dir);
    assert_eq!(m[0].proto, "tcp");
}

#[test]
fn read_mappings_ports2_wins_legacy_fills_uncovered() {
    let dir = unique_dir();
    // 80/tcp present in BOTH channels (ports2 wins → no dup); 5432 only legacy.
    write_spec(
        &dir,
        r#"{"cwd":"/","command":["x"],"env_keys":[],"mounts":[],"detached":true,"created_at_unix":0,"ports":[[8080,80],[5432,5432]],"ports2":[{"host":18080,"container":80,"proto":"tcp"}]}"#,
    );
    let m = read_mappings(&dir);
    // 80/tcp counted once, from ports2 (host 18080), not duplicated by legacy.
    let p80: Vec<_> = m.iter().filter(|x| x.container == 80).collect();
    assert_eq!(p80.len(), 1);
    assert_eq!(p80[0].host, 18080);
    // 5432 still filled from the legacy channel.
    assert!(m.iter().any(|x| x.container == 5432 && x.host == 5432));
}

#[test]
fn read_mappings_absent_or_malformed_spec_is_empty() {
    // No spec.json at all → empty (fail-closed, no fabrication).
    let dir = unique_dir();
    assert!(read_mappings(&dir).is_empty());
    // Garbage spec.json → empty.
    let dir2 = unique_dir();
    write_spec(&dir2, "not json");
    assert!(read_mappings(&dir2).is_empty());
}
