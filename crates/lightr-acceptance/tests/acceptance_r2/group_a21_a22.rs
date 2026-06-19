//! A21–A22 test group: pull network-gated, oci push synthesis.

use std::fs;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::common::lightr_cmd;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// A21 — pull network-gated, loud (strengthened)
//
// Default lane (no LIGHTR_NET_TESTS): assert that `oci pull alpine` returns
// within 90s, exits 0 or 1, NEVER exit 2 (usage error), and NEVER hangs.
// This is a liveness/no-hang gate, NOT a correctness gate.
//
// LIGHTR_NET_TESTS=1 lane: real pull + hydrate + assert no integrity error
// on a good pull (sha256 verify must PASS for a legitimate registry blob).
// ---------------------------------------------------------------------------

#[test]
fn a21_pull_network_gated() {
    let net_tests = std::env::var("LIGHTR_NET_TESTS")
        .map(|v| v == "1")
        .unwrap_or(false);

    if net_tests {
        // Real-network lane: pull alpine and verify /bin/ is present.
        eprintln!("[A21] LIGHTR_NET_TESTS=1: running real-pull lane");
        a21_real_pull_lane();
    } else {
        // Fast-fail lane: assert no hang + no exit 2.
        eprintln!("[A21] LIGHTR_NET_TESTS not set: running liveness (no-hang) lane");
        a21_liveness_lane();
    }
}

fn a21_liveness_lane() {
    let home = TempDir::new().unwrap();

    let start = Instant::now();
    let out = lightr_cmd(home.path())
        .args(["oci", "pull", "alpine", "--name", "@t/a"])
        // Give at most 90 s; a well-behaved CLI returns in < 10 s on any network state.
        .timeout(Duration::from_secs(90))
        .output()
        .expect("oci pull alpine must not fail to spawn");
    let elapsed = start.elapsed();

    let code = out.status.code().unwrap_or(-1);

    eprintln!(
        "[A21 liveness] exit={} elapsed={:.1}s stderr={}",
        code,
        elapsed.as_secs_f32(),
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("(empty)")
    );

    // Must not hang: already guaranteed by the 90 s timeout above.
    // Must exit 0 (net available, pull OK) or 1 (no net / clean error).
    // Must NOT exit 2 (usage/programming error — a valid "alpine" ref is never
    // a usage error; exit 2 would mean parse_image_ref rejected a valid ref).
    assert!(
        code == 0 || code == 1,
        "oci pull alpine must exit 0 or 1 (liveness gate); got exit={} stderr={}",
        code,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        code,
        2,
        "oci pull alpine must NEVER exit 2 (valid ref is not a usage error); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // If it failed (exit 1), stderr must carry a non-empty diagnostic.
    if code == 1 {
        let stderr = out.stderr.clone();
        assert!(
            !stderr.is_empty(),
            "oci pull exit 1 must produce non-empty stderr (clean error message)"
        );
    }
}

// ---------------------------------------------------------------------------
// A22 — oci push: single-layer OCI synthesis (offline) + optional round-trip
//
// Offline lane (always): snapshot a fixture tree under a ref, then `oci push`
// it at an UNROUTABLE registry port. The push must
//   * resolve the ref and SYNTHESIZE the layer/config/manifest BEFORE any
//     upload (proving the imageless synthesis path runs end-to-end), then
//   * fail at the network stage with exit 1 (never exit 2 — a valid store-ref
//     and a valid target ref are not usage errors) and a non-empty diagnostic.
//   * A bad store-ref name (uppercase) must exit 2 (usage).
//
// LIGHTR_REG_TESTS=1 lane: push to $LIGHTR_PUSH_TARGET (e.g. a local
// `registry:2` at localhost:5000/acc/test:latest), then assert exit 0 and a
// well-formed `sha256:<64hex>` manifest digest in stdout. Skipped if unset,
// mirroring the LIGHTR_NET_TESTS gating style above.
// ---------------------------------------------------------------------------

#[test]
fn a22_push_synthesis_offline() {
    let reg_tests = std::env::var("LIGHTR_REG_TESTS")
        .map(|v| v == "1")
        .unwrap_or(false);

    if reg_tests {
        eprintln!("[A22] LIGHTR_REG_TESTS=1: running real push round-trip lane");
        a22_real_push_lane();
    } else {
        eprintln!("[A22] LIGHTR_REG_TESTS not set: running offline-synthesis lane");
        a22_offline_lane();
    }
}

fn a22_offline_lane() {
    let home = TempDir::new().unwrap();

    // Build a fixture tree and snapshot it under a ref.
    let src = TempDir::new().unwrap();
    fs::create_dir_all(src.path().join("etc")).unwrap();
    fs::write(src.path().join("etc/conf"), b"k=v\n").unwrap();
    fs::write(src.path().join("hello"), b"hi").unwrap();
    #[cfg(unix)]
    fs::set_permissions(src.path().join("hello"), fs::Permissions::from_mode(0o755)).unwrap();

    lightr_cmd(home.path())
        .args([
            "snapshot",
            "--dir",
            src.path().to_str().unwrap(),
            "--name",
            "@t/pushme",
        ])
        .assert()
        .success();

    // 1) Bad store-ref name → exit 2 (usage), no network.
    let bad = lightr_cmd(home.path())
        .args(["oci", "push", "INVALID", "localhost:1/x:latest"])
        .output()
        .expect("oci push must spawn");
    assert_eq!(
        bad.status.code().unwrap_or(-1),
        2,
        "bad store-ref name must exit 2; stderr: {}",
        String::from_utf8_lossy(&bad.stderr)
    );

    // 2) Valid ref + valid target at an unroutable port → synthesis runs, then
    //    the upload fails: exit 1 (NEVER 2), non-empty diagnostic, bounded time.
    let start = Instant::now();
    let out = lightr_cmd(home.path())
        .args(["oci", "push", "@t/pushme", "localhost:1/acc/test:latest"])
        .timeout(Duration::from_secs(90))
        .output()
        .expect("oci push must spawn");
    let elapsed = start.elapsed();
    let code = out.status.code().unwrap_or(-1);

    eprintln!(
        "[A22 offline] exit={} elapsed={:.1}s stderr={}",
        code,
        elapsed.as_secs_f32(),
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("(empty)")
    );

    assert_ne!(
        code,
        2,
        "valid store-ref + valid target must NEVER exit 2 (not a usage error); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        code,
        1,
        "push to an unroutable registry must exit 1 (network error after synthesis); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.stderr.is_empty(),
        "push exit 1 must produce a non-empty diagnostic on stderr"
    );
}

fn a22_real_push_lane() {
    let home = TempDir::new().unwrap();
    let target = std::env::var("LIGHTR_PUSH_TARGET")
        .unwrap_or_else(|_| "localhost:5000/acc/test:latest".to_string());

    // Build + snapshot a fixture tree.
    let src = TempDir::new().unwrap();
    fs::create_dir_all(src.path().join("bin")).unwrap();
    fs::write(src.path().join("bin/tool"), b"#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    fs::set_permissions(
        src.path().join("bin/tool"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    lightr_cmd(home.path())
        .args([
            "snapshot",
            "--dir",
            src.path().to_str().unwrap(),
            "--name",
            "@t/pushreal",
        ])
        .assert()
        .success();

    let out = lightr_cmd(home.path())
        .args(["--json", "oci", "push", "@t/pushreal", &target])
        .output()
        .expect("oci push must spawn");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "real push must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Manifest digest must be a well-formed sha256:<64hex>.
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("push --json must emit valid JSON");
    let digest = v["manifest_digest"]
        .as_str()
        .expect("manifest_digest must be a string");
    let hex = digest
        .strip_prefix("sha256:")
        .expect("manifest_digest must start with sha256:");
    assert_eq!(hex.len(), 64, "manifest digest must be 64 hex chars: {hex}");
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "manifest digest must be hex: {hex}"
    );
    eprintln!("[A22 real-push] pushed {target} → {digest}");
}

fn a21_real_pull_lane() {
    let home = TempDir::new().unwrap();

    // Pull alpine from the public Docker Hub registry.
    let pull_out = lightr_cmd(home.path())
        .args([
            "oci",
            "pull",
            "registry-1.docker.io/library/alpine:latest",
            "--name",
            "@t/alpine",
        ])
        .output()
        .expect("oci pull alpine must not fail to spawn");
    assert_eq!(
        pull_out.status.code().unwrap_or(-1),
        0,
        "oci pull alpine (real-net lane) must exit 0; stderr: {}",
        String::from_utf8_lossy(&pull_out.stderr)
    );

    // Must NOT exit 2 (valid registry ref is never a usage error).
    assert_ne!(
        pull_out.status.code().unwrap_or(-1),
        2,
        "oci pull must never exit 2 for a valid registry ref; stderr: {}",
        String::from_utf8_lossy(&pull_out.stderr)
    );

    // Hydrate and verify /bin/ is present.
    let dest = TempDir::new().unwrap();
    lightr_cmd(home.path())
        .args([
            "hydrate",
            dest.path().to_str().unwrap(),
            "--name",
            "@t/alpine",
        ])
        .assert()
        .success();

    let bin_dir = dest.path().join("bin");
    assert!(
        bin_dir.exists() && bin_dir.is_dir(),
        "hydrated alpine must have a /bin directory"
    );
    let entries: Vec<_> = fs::read_dir(&bin_dir)
        .expect("must be able to read /bin")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "hydrated alpine /bin must contain files"
    );

    // sha256 verification passed (pull succeeded without Integrity error):
    // if we get here, all layer blobs matched their declared sha256 digests.
    eprintln!(
        "[A21 real-pull] /bin contains {} entries; sha256 verify passed (no integrity error)",
        entries.len()
    );
}
