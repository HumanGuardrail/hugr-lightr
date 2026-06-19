//! A5–A8 acceptance tests.

use std::fs;
use std::io::Write as _;

use super::common::{fixture_tree, lightr_cmd};
use super::helpers::*;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// A5 — Status: clean→0; modified→1 + name in output; unknown ref→2
// ---------------------------------------------------------------------------
#[test]
fn a5_status() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // snapshot first
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/ws"])
        .assert()
        .success();

    // status on clean tree → exit 0, stdout contains "clean"
    let clean_out = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["status", "--dir", ".", "--name", "@t/ws"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let clean_stdout = String::from_utf8_lossy(&clean_out.stdout);
    assert!(
        clean_stdout.contains("clean"),
        "clean status must print 'clean', got: {clean_stdout}"
    );

    // append a byte to one file
    let modified_file = ws.path().join("level1/sub1/deep1/file_0000.txt");
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&modified_file)
        .unwrap();
    f.write_all(b"X").unwrap();
    drop(f);

    // status on dirty tree → exit 1, stdout contains the modified path
    let dirty_out = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["status", "--dir", ".", "--name", "@t/ws"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let dirty_stdout = String::from_utf8_lossy(&dirty_out.stdout);
    // The spec says "~ <that path>" — check the relative path component appears
    assert!(
        dirty_stdout.contains("file_0000.txt"),
        "dirty status must name the changed file, got: {dirty_stdout}"
    );
    assert!(
        dirty_stdout.contains('~'),
        "dirty status must contain '~' marker, got: {dirty_stdout}"
    );

    // unknown ref → exit 2
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["status", "--dir", ".", "--name", "@t/nope"])
        .assert()
        .code(2);
}

// ---------------------------------------------------------------------------
// A6 — Offline structural: A1 core flow under blocked proxy env vars
// ---------------------------------------------------------------------------
#[test]
fn a6_offline_structural() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // Use a port-9 address that will immediately refuse TCP connections,
    // so any accidental network call surfaces immediately rather than hanging.
    let blocked_proxy = "http://127.0.0.1:9";

    // snapshot with blocked proxy env
    let mut snap_cmd = lightr_cmd(home.path());
    snap_cmd
        .current_dir(ws.path())
        .env("HTTP_PROXY", blocked_proxy)
        .env("HTTPS_PROXY", blocked_proxy)
        .env("ALL_PROXY", blocked_proxy)
        .env("http_proxy", blocked_proxy)
        .env("https_proxy", blocked_proxy)
        .env("all_proxy", blocked_proxy)
        .args(["snapshot", "--dir", ".", "--name", "@t/offline"]);
    snap_cmd.assert().success();

    // hydrate with blocked proxy env
    let mut hyd_cmd = lightr_cmd(home.path());
    hyd_cmd
        .env("HTTP_PROXY", blocked_proxy)
        .env("HTTPS_PROXY", blocked_proxy)
        .env("ALL_PROXY", blocked_proxy)
        .env("http_proxy", blocked_proxy)
        .env("https_proxy", blocked_proxy)
        .env("all_proxy", blocked_proxy)
        .args([
            "hydrate",
            dest.path().to_str().unwrap(),
            "--name",
            "@t/offline",
        ]);
    hyd_cmd.assert().success();

    // compare the trees (reuse A1 helper) — proves the operation succeeded
    compare_trees(ws.path(), dest.path());
}

// ---------------------------------------------------------------------------
// A7 — Integrity fail-closed: flip a byte in an object; hydrate exits 1 + "integrity"
// ---------------------------------------------------------------------------
#[test]
fn a7_integrity_fail_closed() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // snapshot to populate the store
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/ws"])
        .assert()
        .success();

    // find one NON-manifest object file under LIGHTR_HOME/store/objects
    // (the manifest object starts with the LMF1 magic)
    let objects_root = home.path().join("store/objects");
    let object_file = find_object_file(&objects_root, |bytes| !bytes.starts_with(b"LMF1"))
        .expect("must have at least one non-manifest object in store after snapshot");
    corrupt_in_place(&object_file);

    // A7a — paranoid path: `hydrate --verify` re-hashes every object and
    // must fail closed on the corrupt one.
    let out = lightr_cmd(home.path())
        .args([
            "hydrate",
            dest.path().to_str().unwrap(),
            "--name",
            "@t/ws",
            "--verify",
        ])
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(code, 1, "hydrate --verify must exit 1 on integrity failure");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("integrity"),
        "hydrate --verify stderr must contain 'integrity', got: {stderr}"
    );

    // corrupt object file must still exist (not deleted by the binary)
    assert!(
        object_file.exists(),
        "corrupt object file must still exist after failed hydrate: {}",
        object_file.display()
    );

    // A7b — default path stays fail-closed where bytes are READ: the
    // manifest object is always re-hashed, so corrupting IT breaks a
    // default hydrate too (CoW materialization itself trusts the sealed
    // store by design — ADR-0009).
    let manifest_file = find_object_file(&objects_root, |bytes| bytes.starts_with(b"LMF1"))
        .expect("must find the manifest object (LMF1)");
    corrupt_in_place(&manifest_file);

    let dest2 = TempDir::new().unwrap();
    let out2 = lightr_cmd(home.path())
        .args([
            "hydrate",
            dest2.path().join("x").to_str().unwrap(),
            "--name",
            "@t/ws",
        ])
        .output()
        .unwrap();
    let code2 = out2.status.code().unwrap_or(-1);
    assert_eq!(
        code2, 1,
        "default hydrate must exit 1 when the manifest object is corrupt"
    );
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr2.contains("integrity"),
        "default hydrate stderr must contain 'integrity', got: {stderr2}"
    );
}

// ---------------------------------------------------------------------------
// A8 — Agent surface: --json on all 4 verbs produces valid JSON with spec'd keys
// ---------------------------------------------------------------------------
#[test]
fn a8_agent_surface() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    let side = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // --- snapshot --json ---
    let snap_out = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/ws", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let snap_json: serde_json::Value =
        serde_json::from_slice(&snap_out.stdout).expect("snapshot --json must produce valid JSON");
    assert!(
        snap_json.get("root").is_some(),
        "snapshot JSON must have 'root' key, got: {snap_json}"
    );
    assert!(
        snap_json.get("files").is_some(),
        "snapshot JSON must have 'files' key, got: {snap_json}"
    );
    assert!(
        snap_json.get("bytes_total").is_some(),
        "snapshot JSON must have 'bytes_total' key, got: {snap_json}"
    );
    assert!(
        snap_json.get("objects_new").is_some(),
        "snapshot JSON must have 'objects_new' key, got: {snap_json}"
    );

    // --- hydrate --json ---
    let hyd_out = lightr_cmd(home.path())
        .args([
            "hydrate",
            dest.path().to_str().unwrap(),
            "--name",
            "@t/ws",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let hyd_json: serde_json::Value =
        serde_json::from_slice(&hyd_out.stdout).expect("hydrate --json must produce valid JSON");
    assert!(
        hyd_json.get("root").is_some(),
        "hydrate JSON must have 'root' key, got: {hyd_json}"
    );
    assert!(
        hyd_json.get("files").is_some(),
        "hydrate JSON must have 'files' key, got: {hyd_json}"
    );
    assert!(
        hyd_json.get("bytes_total").is_some(),
        "hydrate JSON must have 'bytes_total' key, got: {hyd_json}"
    );
    assert!(
        hyd_json.get("rung").is_some(),
        "hydrate JSON must have 'rung' key, got: {hyd_json}"
    );

    // --- status --json ---
    let stat_out = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["status", "--dir", ".", "--name", "@t/ws", "--json"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let stat_json: serde_json::Value =
        serde_json::from_slice(&stat_out.stdout).expect("status --json must produce valid JSON");
    assert!(
        stat_json.get("clean").is_some(),
        "status JSON must have 'clean' key, got: {stat_json}"
    );
    assert!(
        stat_json.get("added").is_some(),
        "status JSON must have 'added' key, got: {stat_json}"
    );
    assert!(
        stat_json.get("removed").is_some(),
        "status JSON must have 'removed' key, got: {stat_json}"
    );
    assert!(
        stat_json.get("changed").is_some(),
        "status JSON must have 'changed' key, got: {stat_json}"
    );

    // --- run --json ---
    // Child stdout must be intact on stdout; final stderr line starts "lightr-json: "
    // and parses to {key, hit, exit_code}.
    let sidefx = side.path().join("sidefx");
    let cmd_str = format!(
        "echo hello_from_run; echo $$ >> {}",
        sidefx.to_str().unwrap()
    );
    let run_out = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args([
            "run",
            "--dir",
            ".",
            "--input",
            ws.path().to_str().unwrap(),
            "--json",
            "--",
            "/bin/sh",
            "-c",
            &cmd_str,
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    // child stdout must be intact
    let run_stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(
        run_stdout.contains("hello_from_run"),
        "run --json must pass child stdout through intact, got: {run_stdout}"
    );

    // final stderr line must start with "lightr-json: " and parse to JSON
    let run_stderr = String::from_utf8_lossy(&run_out.stderr);
    let json_line = run_stderr
        .lines()
        .rfind(|l| l.starts_with("lightr-json: "))
        .unwrap_or_else(|| {
            panic!(
                "run --json stderr must contain a line starting 'lightr-json: ', got: {run_stderr}"
            )
        });
    let json_payload = json_line.trim_start_matches("lightr-json: ");
    let run_json: serde_json::Value =
        serde_json::from_str(json_payload).expect("lightr-json payload must be valid JSON");
    assert!(
        run_json.get("key").is_some(),
        "run JSON must have 'key', got: {run_json}"
    );
    assert!(
        run_json.get("hit").is_some(),
        "run JSON must have 'hit', got: {run_json}"
    );
    assert!(
        run_json.get("exit_code").is_some(),
        "run JSON must have 'exit_code', got: {run_json}"
    );
}
