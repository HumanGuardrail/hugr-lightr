//! Arch-selection, streaming-apply, timeout, and network-gated pull tests.

use super::{make_layer, make_layout, tmp_store_and_home, ENV_LOCK};
use crate::oci::layer::{apply_and_snapshot, layer_timeout_secs, LayerBlob};
use crate::oci::model::{OciDescriptor, OciPlatform};
use crate::oci::pull::pull;
use crate::oci::reference::pick_from_manifest_list;
use crate::oci::util::host_arch;
use lightr_core::LightrError;
use std::fs;
use tempfile::TempDir;

// ── WP-A-pull: arch selection tests ───────────────────────────────────────

/// Synthetic manifest list with amd64 + arm64: host picks correctly.
#[test]
fn test_arch_selection_picks_host() {
    fn make_desc(os: &str, arch: &str, digest: &str) -> OciDescriptor {
        OciDescriptor {
            digest: digest.to_string(),
            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
            size: 0,
            platform: Some(OciPlatform {
                os: os.to_string(),
                architecture: arch.to_string(),
            }),
        }
    }

    let manifests = vec![
        make_desc("linux", "amd64", "sha256:aaaaaa"),
        make_desc("linux", "arm64", "sha256:bbbbbb"),
        make_desc("windows", "amd64", "sha256:cccccc"),
    ];

    // The host_arch() function reads std::env::consts::ARCH.
    let arch = host_arch();
    let chosen = pick_from_manifest_list(&manifests).unwrap();
    let chosen_arch = chosen
        .platform
        .as_ref()
        .map(|p| p.architecture.as_str())
        .unwrap_or("");
    let chosen_os = chosen
        .platform
        .as_ref()
        .map(|p| p.os.as_str())
        .unwrap_or("");

    // Must pick linux AND the correct arch (or amd64 fallback).
    assert_eq!(chosen_os, "linux", "must pick a linux entry");
    if arch == "amd64" || arch == "arm64" {
        assert_eq!(
            chosen_arch, arch,
            "must pick the host arch {arch}, got {chosen_arch}"
        );
    } else {
        // Unknown host: falls back to amd64.
        assert_eq!(chosen_arch, "amd64", "unknown host must fall back to amd64");
    }
}

/// Missing host arch → falls back to amd64.
#[test]
fn test_arch_selection_fallback_to_amd64() {
    fn make_desc(os: &str, arch: &str) -> OciDescriptor {
        OciDescriptor {
            digest: format!("sha256:{os}-{arch}"),
            media_type: String::new(),
            size: 0,
            platform: Some(OciPlatform {
                os: os.to_string(),
                architecture: arch.to_string(),
            }),
        }
    }

    // Only amd64 (no arm64); on an arm64 host this tests the fallback.
    let manifests = vec![make_desc("linux", "amd64"), make_desc("windows", "amd64")];

    let chosen = pick_from_manifest_list(&manifests).unwrap();
    let arch = chosen
        .platform
        .as_ref()
        .map(|p| p.architecture.as_str())
        .unwrap_or("");
    let os = chosen
        .platform
        .as_ref()
        .map(|p| p.os.as_str())
        .unwrap_or("");
    assert_eq!(os, "linux");
    assert_eq!(arch, "amd64");
}

/// No linux entries → error naming available arches.
#[test]
fn test_arch_selection_no_linux_entry_errors() {
    fn make_desc(os: &str, arch: &str) -> OciDescriptor {
        OciDescriptor {
            digest: format!("sha256:{os}-{arch}"),
            media_type: String::new(),
            size: 0,
            platform: Some(OciPlatform {
                os: os.to_string(),
                architecture: arch.to_string(),
            }),
        }
    }

    let manifests = vec![make_desc("windows", "amd64"), make_desc("darwin", "arm64")];

    let err = pick_from_manifest_list(&manifests).unwrap_err();
    assert!(
        matches!(err, LightrError::InvalidManifest(_)),
        "no linux entry must be InvalidManifest"
    );
    if let LightrError::InvalidManifest(msg) = err {
        assert!(
            msg.contains("no linux entry"),
            "error must name the problem; got: {msg}"
        );
        // Must list available arches.
        assert!(
            msg.contains("windows") || msg.contains("darwin"),
            "error must list available arches; got: {msg}"
        );
    }
}

// ── Streaming-apply path: ≥64 MiB uncompressed layer via LayerBlob::File ──

/// Verify that `apply_layers` streams a layer from a file (the `LayerBlob::File`
/// path taken by `pull`) without buffering the whole layer into a `Vec<u8>`.
///
/// # What this test proves
///
/// - `apply_layers` is called with `LayerBlob::File`, exercising `open_reader`'s
///   file branch (the path that was previously doing `fs::read` into a full Vec).
/// - A ≥64 MiB **uncompressed** plain-tar layer (incompressible content: a 4 KiB
///   XOR-chained pseudo-random pattern repeated to fill 64 MiB + 1 B) applies
///   correctly and the resulting file has the right size and first/last bytes.
/// - The layer file on disk is genuinely large (asserted below), confirming the
///   on-disk size is not compressed away.
///
/// # What this test does NOT prove
///
/// A unit test cannot instrument RAM usage; we cannot assert a hard RSS bound.
/// The claim "no whole-layer Vec" is guaranteed by code structure: `open_reader`
/// never calls `fs::read`, and `tar::Archive` iterates entries through its own
/// bounded I/O buffer.  Code review of `open_reader` + `apply_layers` is the
/// authoritative check for that invariant.
#[test]
fn test_apply_streams_without_buffering_whole_layer() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    // Build incompressible content: a 4 KiB pattern generated by a simple XOR
    // chain so gzip cannot reduce it to a few KiB.
    const FILE_SIZE: usize = 64 * 1024 * 1024 + 1; // 64 MiB + 1
    let mut content = vec![0u8; FILE_SIZE];
    // Seed the pattern with values that resist gzip's LZ77/Huffman compression.
    let mut v: u8 = 0xA5;
    for b in content.iter_mut() {
        v = v.wrapping_mul(131).wrapping_add(17);
        *b = v;
    }
    let first_byte = content[0];
    let last_byte = content[FILE_SIZE - 1];

    // Build a plain (uncompressed) tar — no gzip — so the on-disk layer file
    // is also ≥64 MiB.  `open_reader` handles this: it peeks 2 bytes, sees no
    // gzip magic, and passes the raw reader straight to `tar::Archive`.
    let mut tar_bytes: Vec<u8> = Vec::new();
    {
        let mut tar_b = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_path("bigfile.bin").unwrap();
        header.set_mode(0o644);
        header.set_size(content.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar_b.append(&header, content.as_slice()).unwrap();
        tar_b.finish().unwrap();
    }
    // The on-disk tar must be genuinely large (tar overhead ≈ 512 B per entry).
    assert!(
        tar_bytes.len() > FILE_SIZE,
        "tar must be at least as large as the file content"
    );

    // Write the layer tar to a file, then hand it to apply_layers via
    // LayerBlob::File — this is the exact path taken by `pull`.
    let layer_file = tmp.path().join("layer.tar");
    fs::write(&layer_file, &tar_bytes).unwrap();
    // Confirm the on-disk file is large.
    let on_disk_len = fs::metadata(&layer_file).unwrap().len() as usize;
    assert!(
        on_disk_len > FILE_SIZE,
        "on-disk layer must be ≥{FILE_SIZE} bytes, got {on_disk_len}"
    );

    // Use apply_and_snapshot with LayerBlob::File — the streaming path.
    let blobs = vec![LayerBlob::File(layer_file)];
    let report = apply_and_snapshot(blobs, 1, &store, "stream-apply-test").unwrap();
    assert_eq!(report.layers, 1, "must report 1 layer");

    // Hydrate and verify correctness of the applied content.
    let hydrate_dir = tmp.path().join("hydrated-stream");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "stream-apply-test").unwrap();

    let big = hydrate_dir.join("bigfile.bin");
    assert!(
        big.exists(),
        "bigfile.bin must be present after streaming apply"
    );
    let meta = fs::metadata(&big).unwrap();
    assert_eq!(
        meta.len() as usize,
        FILE_SIZE,
        "bigfile.bin must be exactly {FILE_SIZE} bytes"
    );
    // Spot-check first and last bytes to confirm content fidelity.
    let hydrated = fs::read(&big).unwrap();
    assert_eq!(hydrated[0], first_byte, "first byte must match");
    assert_eq!(hydrated[FILE_SIZE - 1], last_byte, "last byte must match");
}

// ── Fix 2: wall-clock guard on apply_layers ───────────────────────────────

/// A small synthetic layer applies successfully well within the default
/// 600 s deadline.  This is the sanity check that the guard does not trip
/// on normal workloads.
#[test]
fn test_apply_layers_normal_layer_within_deadline() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let layer = make_layer(&[
        ("hello.txt", b"hello world", 0o644),
        ("etc/conf", b"key=value\n", 0o644),
    ]);
    let layout_dir = make_layout(tmp.path(), &[layer]);
    let result = crate::oci::import::import_layout(&layout_dir, &store, "timeout-sanity");
    assert!(
        result.is_ok(),
        "small layer must apply within default deadline; got: {:?}",
        result.err()
    );
}

/// `LIGHTR_LAYER_TIMEOUT_SECS` env override is parsed correctly.
/// We verify `layer_timeout_secs()` returns the overridden value when the
/// var is set to a valid positive integer, and returns the default (600)
/// when it is absent or invalid.
///
/// NOTE: A true slow-tar stress test (force a timeout mid-extraction) is
/// out of scope for this wave — it would require injecting latency into the
/// tar reader, which would add test infrastructure complexity without adding
/// determinism.  The guard's correctness is verified by code review of the
/// entry-count sampling logic and the deadline comparison.
#[test]
fn test_layer_timeout_env_override_parsed() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    // Default: no env var set → 600.
    std::env::remove_var("LIGHTR_LAYER_TIMEOUT_SECS");
    assert_eq!(
        layer_timeout_secs(),
        600,
        "default timeout must be 600 s when LIGHTR_LAYER_TIMEOUT_SECS is unset"
    );

    // Valid override: 120 s.
    std::env::set_var("LIGHTR_LAYER_TIMEOUT_SECS", "120");
    assert_eq!(
        layer_timeout_secs(),
        120,
        "env override of 120 must be respected"
    );

    // Invalid value (non-integer) → falls back to default 600.
    std::env::set_var("LIGHTR_LAYER_TIMEOUT_SECS", "not-a-number");
    assert_eq!(
        layer_timeout_secs(),
        600,
        "non-integer env value must fall back to 600"
    );

    // Zero is invalid (must be > 0) → falls back to default 600.
    std::env::set_var("LIGHTR_LAYER_TIMEOUT_SECS", "0");
    assert_eq!(
        layer_timeout_secs(),
        600,
        "zero env value must fall back to 600"
    );

    // Restore env so other tests are unaffected.
    std::env::remove_var("LIGHTR_LAYER_TIMEOUT_SECS");
}

/// pull: network-gated test.
/// Without LIGHTR_NET_TESTS=1: no-op (asserts nothing network, fast).
/// With LIGHTR_NET_TESTS=1: hits docker.io alpine:latest and verifies /bin/ exists.
#[test]
fn test_pull_alpine_network_gated() {
    if std::env::var("LIGHTR_NET_TESTS").is_err() {
        eprintln!(
            "[lightr-oci] pull test SKIPPED — set LIGHTR_NET_TESTS=1 to run against docker.io"
        );
        return;
    }

    // Network lane: real pull of alpine:latest
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    eprintln!("[lightr-oci] LIGHTR_NET_TESTS=1 — pulling docker.io/library/alpine:latest");

    let report = pull("alpine:latest", &store, "alpine-test").unwrap();
    assert!(report.layers > 0, "alpine must have at least 1 layer");

    let hydrate_dir = tmp.path().join("hydrated-alpine");
    fs::create_dir_all(&hydrate_dir).unwrap();
    lightr_index::hydrate(&hydrate_dir, &store, "alpine-test").unwrap();

    assert!(
        hydrate_dir.join("bin").exists(),
        "hydrated alpine must contain /bin"
    );
    eprintln!("[lightr-oci] pull test PASSED (network lane)");
}
