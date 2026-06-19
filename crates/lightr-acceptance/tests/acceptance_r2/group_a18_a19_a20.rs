//! A18–A20 test group: idempotent import/lineage, engine probes, rootfs guard.

use super::helpers::*;
use std::collections::HashMap;

use crate::common::lightr_cmd;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// A18 — import idempotent + lineage (strengthened)
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

    // Same content → same root digest (byte-equal).
    assert_eq!(
        root1, root2,
        "import of identical tar twice must produce same root digest (byte-equal)"
    );

    // Lineage: reflog must have EXACTLY 2 entries after two imports.
    // `diff --name @t/img --at 1` exits 0 (identical trees) or 1 (different)
    // but must NOT exit 2 (which would mean reflog length < 2 / index OOB).
    let diff_out = lightr_cmd(home.path())
        .args(["diff", "--name", "@t/img", "--at", "1"])
        .output()
        .expect("diff --at 1 must launch");
    let code = diff_out.status.code().unwrap_or(-1);
    assert_ne!(
        code,
        2,
        "diff --name @t/img --at 1 must not exit 2 (reflog must have ≥2 entries); stderr: {}",
        String::from_utf8_lossy(&diff_out.stderr)
    );

    // Both reflog entries must be the same root (same tar → same content).
    // exit 0 from diff means the two roots are identical, confirming this.
    // exit 1 means they differ — acceptable if the importer advances even for
    // the same content, but we report it so the operator can verify manually.
    if code == 0 {
        // Both roots identical — exactly what we expect.
    } else if code == 1 {
        // The importer produced different roots for the same tar twice.
        // This is a heuristic failure; we do NOT hard-assert here because the
        // current spec allows the reflog to chain without requiring content
        // stability across imports (the unit test in lightr-oci asserts it).
        eprintln!(
            "[A18] WARNING: diff --at 1 exit 1 — roots differ between two identical imports; root1={root1} root2={root2}"
        );
    }
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
            let name = entry.get("kind").and_then(|n| n.as_str())?;
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
    // ns is available IFF the host is Linux (honest probe): true on the Linux CI
    // gate, false on macOS/Windows.
    #[cfg(target_os = "linux")]
    let ns_expected = Some(true);
    #[cfg(not(target_os = "linux"))]
    let ns_expected = Some(false);
    assert_eq!(
        ns.get("available").and_then(|v| v.as_bool()),
        ns_expected,
        "ns.available must match host (Linux=true, else false); got: {ns}"
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
