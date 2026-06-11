//! A17–A21 per build-spec-r2.md §5 — authored by WP-R2-W4 (red-first).
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo check -p lightr-acceptance --all-targets.
//! The binary is expected to have the R2 verbs merged in; these tests are
//! authored red-first (compile-only gate until the post-merge green run).
//! Do NOT weaken assertions.
//!
//! Fixture form for A17: docker-save TAR. The fixture contains manifest.json
//! plus two uncompressed layer tars (built with the `tar` crate). No sha2 dep
//! is needed: docker-save manifests reference layers by filename, not digest.
//! `flate2` is added as a dev-dep per spec authorisation; layers are kept
//! uncompressed in this fixture so `flate2` is not called directly.

#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::lightr_cmd;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// OCI fixture builder — docker-save TAR form
// ---------------------------------------------------------------------------

/// A single file entry to add to a layer tar.
struct TarEntry<'a> {
    path: &'a str,
    content: &'a [u8],
    mode: u32,
}

/// Build an uncompressed layer tar in memory from the given entries.
///
/// Whiteout entries are added as empty files at the given path (e.g.
/// `etc/.wh.drop` to delete `etc/drop` from lower layers).
fn build_layer_tar(entries: &[TarEntry<'_>]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut buf);
        for e in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(e.content.len() as u64);
            header.set_mode(e.mode);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            ar.append_data(&mut header, e.path, e.content).unwrap();
        }
        ar.finish().unwrap();
    }
    buf
}

/// Build a minimal docker-save tar layout at `<dir>/image.tar`.
///
/// Layout inside image.tar:
///   manifest.json           — [{Config, RepoTags, Layers:[layer1.tar, layer2.tar]}]
///   layer1.tar              — adds etc/keep (mode 0644), etc/drop (mode 0644),
///                             bin/tool (mode 0755)
///   layer2.tar              — whiteout etc/.wh.drop + adds app/hello (mode 0755)
///   <fake-config-hash>.json — minimal OCI config blob (required by some importers)
///
/// Why docker-save instead of OCI layout with sha2 digests: the spec
/// (§5 authoring-law) explicitly permits this form to avoid adding sha2 as a
/// dev-dep. The `lightr-oci` importer autodetects this form via manifest.json.
pub fn make_oci_layout(dir: &Path) -> PathBuf {
    // --- layer 1: etc/keep + etc/drop + bin/tool ---
    let layer1_data = build_layer_tar(&[
        TarEntry {
            path: "etc/keep",
            content: b"k",
            mode: 0o644,
        },
        TarEntry {
            path: "etc/drop",
            content: b"d",
            mode: 0o644,
        },
        TarEntry {
            path: "bin/tool",
            content: b"#!/bin/sh\n",
            mode: 0o755,
        },
    ]);

    // --- layer 2: whiteout etc/drop + add app/hello ---
    let layer2_data = build_layer_tar(&[
        // Whiteout: empty file named ".wh.<basename>" in the same directory.
        TarEntry {
            path: "etc/.wh.drop",
            content: b"",
            mode: 0o644,
        },
        TarEntry {
            path: "app/hello",
            content: b"hi",
            mode: 0o755,
        },
    ]);

    // --- minimal config blob (empty JSON object suffices for the importer) ---
    let config_data = b"{}";
    let config_name = "config.json";

    // --- manifest.json (docker-save format) ---
    let manifest_json = serde_json::json!([{
        "Config": config_name,
        "RepoTags": ["acceptance-test:latest"],
        "Layers": ["layer1.tar", "layer2.tar"]
    }]);
    let manifest_bytes = serde_json::to_vec(&manifest_json).unwrap();

    // --- assemble image.tar ---
    let image_tar_path = dir.join("image.tar");
    let file = fs::File::create(&image_tar_path).unwrap();
    let mut ar = tar::Builder::new(file);

    let mut append = |name: &str, data: &[u8]| {
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(data.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_entry_type(tar::EntryType::Regular);
        hdr.set_cksum();
        ar.append_data(&mut hdr, name, data).unwrap();
    };

    append("manifest.json", &manifest_bytes);
    append("layer1.tar", &layer1_data);
    append("layer2.tar", &layer2_data);
    append(config_name, config_data);

    ar.finish().unwrap();

    image_tar_path
}

// ---------------------------------------------------------------------------
// Helper: parse `root=<hex>` from stdout.
// ---------------------------------------------------------------------------
fn parse_root_from_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        for tok in line.split_whitespace() {
            if let Some(hex) = tok.strip_prefix("root=") {
                if hex.len() >= 16 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return hex.to_owned();
                }
            }
        }
    }
    panic!(
        "could not find 'root=<16+hex>' in stdout:\n{}",
        String::from_utf8_lossy(stdout)
    );
}

// ---------------------------------------------------------------------------
// A17 — OCI import roundtrip (offline)
// ---------------------------------------------------------------------------

#[test]
fn a17_oci_import_roundtrip() {
    let home = TempDir::new().unwrap();
    let fixture_dir = TempDir::new().unwrap();
    let image_tar = make_oci_layout(fixture_dir.path());

    // Import the docker-save tar.
    let import_out = lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            image_tar.to_str().unwrap(),
            "--name",
            "@t/img",
        ])
        .output()
        .expect("oci import must not fail to launch");
    assert_eq!(
        import_out.status.code().unwrap_or(-1),
        0,
        "oci import must exit 0; stderr: {}",
        String::from_utf8_lossy(&import_out.stderr)
    );

    // Hydrate and verify post-whiteout tree.
    let dest = TempDir::new().unwrap();
    lightr_cmd(home.path())
        .args(["hydrate", dest.path().to_str().unwrap(), "--name", "@t/img"])
        .assert()
        .success();

    // etc/keep must be present with content "k".
    let keep_path = dest.path().join("etc/keep");
    assert!(keep_path.exists(), "etc/keep must be present after hydrate");
    assert_eq!(
        fs::read(&keep_path).unwrap(),
        b"k",
        "etc/keep content must be \"k\""
    );

    // etc/drop must be ABSENT (whiteout applied).
    let drop_path = dest.path().join("etc/drop");
    assert!(
        !drop_path.exists(),
        "etc/drop must be absent (whiteout applied)"
    );

    // bin/tool must be present with mode 0755.
    let tool_path = dest.path().join("bin/tool");
    assert!(tool_path.exists(), "bin/tool must be present after hydrate");
    let tool_mode = fs::metadata(&tool_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        tool_mode, 0o755,
        "bin/tool mode must be 0755; got {:o}",
        tool_mode
    );

    // app/hello must be present with content "hi" and mode 0755.
    let hello_path = dest.path().join("app/hello");
    assert!(
        hello_path.exists(),
        "app/hello must be present after hydrate"
    );
    assert_eq!(
        fs::read(&hello_path).unwrap(),
        b"hi",
        "app/hello content must be \"hi\""
    );
    let hello_mode = fs::metadata(&hello_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        hello_mode, 0o755,
        "app/hello mode must be 0755; got {:o}",
        hello_mode
    );
}

// ---------------------------------------------------------------------------
// A18 — import idempotent + lineage
// ---------------------------------------------------------------------------

#[test]
fn a18_import_idempotent_lineage() {
    let home = TempDir::new().unwrap();
    let fixture_dir = TempDir::new().unwrap();
    let image_tar = make_oci_layout(fixture_dir.path());

    // First import.
    let out1 = lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            image_tar.to_str().unwrap(),
            "--name",
            "@t/img",
        ])
        .output()
        .expect("first oci import must not fail to launch");
    assert_eq!(
        out1.status.code().unwrap_or(-1),
        0,
        "first oci import must exit 0; stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let root1 = parse_root_from_stdout(&out1.stdout);

    // Second import of the same tar.
    let out2 = lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            image_tar.to_str().unwrap(),
            "--name",
            "@t/img",
        ])
        .output()
        .expect("second oci import must not fail to launch");
    assert_eq!(
        out2.status.code().unwrap_or(-1),
        0,
        "second oci import must exit 0; stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let root2 = parse_root_from_stdout(&out2.stdout);

    // Same content → same root digest.
    assert_eq!(
        root1, root2,
        "import of identical tar twice must produce same root digest"
    );

    // Lineage: the ref-log must have length ≥ 2.
    // `diff --name @t/img --at 1` should exit 0 (identical) confirming two
    // entries exist in the reflog.
    let diff_out = lightr_cmd(home.path())
        .args(["diff", "--name", "@t/img", "--at", "1"])
        .output()
        .expect("diff --at 1 must launch");
    // exit 0 = identical trees (both imports of the same tar → same root).
    // exit 1 = different (unexpected but allowed if the importer advances the ref).
    // exit 2 = ref not found or index out of range → reflog is NOT len≥2 → fail.
    let code = diff_out.status.code().unwrap_or(-1);
    assert_ne!(
        code,
        2,
        "diff --name @t/img --at 1 must not exit 2 (reflog must have ≥2 entries); stderr: {}",
        String::from_utf8_lossy(&diff_out.stderr)
    );
}

// ---------------------------------------------------------------------------
// A19 — engine probes honest
// ---------------------------------------------------------------------------

#[test]
fn a19_engine_probes_honest() {
    let home = TempDir::new().unwrap();

    // engine ls --json must exit 0 and return a JSON array.
    let ls_out = lightr_cmd(home.path())
        .args(["engine", "ls", "--json"])
        .output()
        .expect("engine ls --json must launch");
    assert_eq!(
        ls_out.status.code().unwrap_or(-1),
        0,
        "engine ls --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&ls_out.stderr)
    );

    let arr: serde_json::Value =
        serde_json::from_slice(&ls_out.stdout).expect("engine ls --json must emit valid JSON");
    let arr = arr
        .as_array()
        .expect("engine ls --json must emit a JSON array");

    // Build a map from engine name → caps object for easy lookup.
    let caps: HashMap<&str, &serde_json::Value> = arr
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(|n| n.as_str())?;
            Some((name, entry))
        })
        .collect();

    // native must be present and available.
    let native = caps
        .get("native")
        .unwrap_or_else(|| panic!("engine ls must include 'native'; got: {:?}", arr));
    assert_eq!(
        native.get("available").and_then(|v| v.as_bool()),
        Some(true),
        "native.available must be true; got: {native}"
    );

    // ns must be present and unavailable on macOS, with "Linux" in detail.
    let ns = caps
        .get("ns")
        .unwrap_or_else(|| panic!("engine ls must include 'ns'; got: {:?}", arr));
    assert_eq!(
        ns.get("available").and_then(|v| v.as_bool()),
        Some(false),
        "ns.available must be false on macOS; got: {ns}"
    );
    let ns_detail = ns
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("ns.detail must be a string; got: {ns}"));
    assert!(
        ns_detail.to_lowercase().contains("linux"),
        "ns.detail must mention 'Linux' (case-insensitive); got: \"{}\"",
        ns_detail
    );

    // vz must be present and unavailable (feature off in default build),
    // with an actionable detail.
    let vz = caps
        .get("vz")
        .unwrap_or_else(|| panic!("engine ls must include 'vz'; got: {:?}", arr));
    assert_eq!(
        vz.get("available").and_then(|v| v.as_bool()),
        Some(false),
        "vz.available must be false (feature 'vz' off); got: {vz}"
    );
    let vz_detail = vz
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("vz.detail must be a string; got: {vz}"));
    // Actionable: must not be empty.
    assert!(
        !vz_detail.trim().is_empty(),
        "vz.detail must be non-empty (actionable); got: {vz}"
    );

    // run --engine ns -- /bin/true must exit 2 with ns probe detail in stderr.
    let run_ns = lightr_cmd(home.path())
        .args(["run", "--engine", "ns", "--", "/bin/true"])
        .output()
        .expect("run --engine ns must launch");
    assert_eq!(
        run_ns.status.code().unwrap_or(-1),
        2,
        "run --engine ns must exit 2 on macOS; stderr: {}",
        String::from_utf8_lossy(&run_ns.stderr)
    );
    let run_ns_stderr = String::from_utf8_lossy(&run_ns.stderr);
    assert!(
        run_ns_stderr.to_lowercase().contains("linux"),
        "run --engine ns stderr must contain probe detail mentioning 'Linux'; got: \"{}\"",
        run_ns_stderr
    );

    // run --engine vz -- /bin/true must exit 2.
    let run_vz = lightr_cmd(home.path())
        .args(["run", "--engine", "vz", "--", "/bin/true"])
        .output()
        .expect("run --engine vz must launch");
    assert_eq!(
        run_vz.status.code().unwrap_or(-1),
        2,
        "run --engine vz must exit 2 (feature off); stderr: {}",
        String::from_utf8_lossy(&run_vz.stderr)
    );
}

// ---------------------------------------------------------------------------
// A20 — rootfs guard
// ---------------------------------------------------------------------------

#[test]
fn a20_rootfs_guard() {
    let home = TempDir::new().unwrap();
    let fixture_dir = TempDir::new().unwrap();
    let image_tar = make_oci_layout(fixture_dir.path());

    // Import the image first so @t/img is a valid ref.
    lightr_cmd(home.path())
        .args([
            "oci",
            "import",
            image_tar.to_str().unwrap(),
            "--name",
            "@t/img",
        ])
        .assert()
        .success();

    // run --engine native --rootfs @t/img -- /bin/true must exit 2 and stderr
    // must mention "rootfs" (native engine does not support rootfs isolation).
    let run_out = lightr_cmd(home.path())
        .args([
            "run",
            "--engine",
            "native",
            "--rootfs",
            "@t/img",
            "--",
            "/bin/true",
        ])
        .output()
        .expect("run --engine native --rootfs must launch");
    assert_eq!(
        run_out.status.code().unwrap_or(-1),
        2,
        "run --engine native --rootfs must exit 2; stderr: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(
        stderr.to_lowercase().contains("rootfs"),
        "run --engine native --rootfs stderr must mention 'rootfs'; got: \"{}\"",
        stderr
    );
}

// ---------------------------------------------------------------------------
// A21 — pull network-gated, loud
//
// Default lane (no LIGHTR_NET_TESTS): assert that `oci pull alpine` returns
// within 90s and exits with code 0 or 1 — NEVER exit 2 and NEVER hang.
// This is a liveness/no-hang gate, NOT a correctness gate. Exit 0 means the
// host has network and the pull succeeded; exit 1 means no network or a clean
// registry error; both are acceptable. Exit 2 is a usage-error (programming
// error in the test or CLI) and must never occur.
//
// LIGHTR_NET_TESTS=1 lane: real pull + hydrate lists bin/ to verify correctness.
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
    // Must NOT exit 2 (usage/programming error).
    assert!(
        code == 0 || code == 1,
        "oci pull alpine must exit 0 or 1 (liveness gate); got exit={} stderr={}",
        code,
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

    eprintln!("[A21 real-pull] /bin contains {} entries", entries.len());
}
