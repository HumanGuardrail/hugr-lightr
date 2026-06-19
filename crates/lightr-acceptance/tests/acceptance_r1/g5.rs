use super::common::*;
use super::helpers::*;

use std::fs;
use std::time::Duration;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// a8b — --json payloads: gc, undo, diff, run
// ---------------------------------------------------------------------------
#[test]
fn a8b_json_payloads() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    fixture_tree(ws.path());

    // Snapshot v1.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // Snapshot v2 (identical — tests undo).
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // --- gc --json ---
    let gc_out = lightr_cmd(home.path())
        .args(["gc", "--json"])
        .output()
        .expect("gc --json must launch");
    assert_eq!(
        gc_out.status.code().unwrap_or(-1),
        0,
        "gc --json must exit 0"
    );
    let gc_json: serde_json::Value =
        serde_json::from_slice(&gc_out.stdout).expect("gc --json must emit valid JSON");
    for key in &[
        "objects_total",
        "reachable",
        "swept",
        "bytes_freed",
        "run_dirs_removed",
    ] {
        assert!(
            gc_json.get(key).is_some(),
            "gc --json must have '{}' key; got: {gc_json}",
            key
        );
    }

    // --- undo --json ---
    let undo_out = lightr_cmd(home.path())
        .args(["undo", "--name", "@t/x", "--json"])
        .output()
        .expect("undo --json must launch");
    assert_eq!(
        undo_out.status.code().unwrap_or(-1),
        0,
        "undo --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&undo_out.stderr)
    );
    let undo_json: serde_json::Value =
        serde_json::from_slice(&undo_out.stdout).expect("undo --json must emit valid JSON");
    assert!(
        undo_json.get("name").is_some(),
        "undo --json must have 'name' key; got: {undo_json}"
    );
    assert!(
        undo_json.get("root").is_some(),
        "undo --json must have 'root' key; got: {undo_json}"
    );

    // --- diff --json (different versions) ---
    // Snapshot v3 with a changed file.
    let modified = ws.path().join("level1/sub1/deep1/file_0000.txt");
    fs::write(&modified, b"a8b diff changed content").unwrap();
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    let diff_out = lightr_cmd(home.path())
        .args(["diff", "--name", "@t/x", "--at", "1", "--json"])
        .output()
        .expect("diff --json must launch");
    // exit 1 = different
    assert_eq!(
        diff_out.status.code().unwrap_or(-1),
        1,
        "diff --json must exit 1 (different); stderr: {}",
        String::from_utf8_lossy(&diff_out.stderr)
    );
    let diff_json: serde_json::Value =
        serde_json::from_slice(&diff_out.stdout).expect("diff --json must emit valid JSON");
    for key in &["added", "removed", "changed"] {
        assert!(
            diff_json.get(key).is_some(),
            "diff --json must have '{}' key; got: {diff_json}",
            key
        );
    }

    // --- run --json stderr line ---
    let run_out = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["run", "--dir", ".", "--json", "--", "/bin/echo", "hi"])
        .output()
        .expect("run --json must launch");
    assert_eq!(
        run_out.status.code().unwrap_or(-1),
        0,
        "run --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    let run_stderr = String::from_utf8_lossy(&run_out.stderr);
    let json_line = run_stderr
        .lines()
        .find(|l| l.starts_with("lightr-json:"))
        .unwrap_or_else(|| {
            panic!("run --json stderr must contain 'lightr-json: ...' line; got:\n{run_stderr}")
        });
    let json_part = json_line.strip_prefix("lightr-json: ").unwrap_or_else(|| {
        panic!("lightr-json line must start with 'lightr-json: '; got: {json_line}")
    });
    let run_json: serde_json::Value =
        serde_json::from_str(json_part).expect("run --json payload must be valid JSON");
    assert!(
        run_json.get("key").is_some(),
        "run --json payload must have 'key'; got: {run_json}"
    );
    assert!(
        run_json.get("hit").is_some(),
        "run --json payload must have 'hit'; got: {run_json}"
    );
    assert!(
        run_json.get("exit_code").is_some(),
        "run --json payload must have 'exit_code'; got: {run_json}"
    );
}

// ---------------------------------------------------------------------------
// a9c — --mount grammar rejections
// ---------------------------------------------------------------------------
#[test]
fn a9c_mount_grammar() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    fixture_tree(ws.path());

    // Snapshot @t/x so grammar failures aren't masked by missing-ref errors.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // --mount badNAME!:x → invalid ref name → exit 2.
    let out1 = lightr_cmd(home.path())
        .args(["run", "--mount", "badNAME!:x", "--", "true"])
        .output()
        .expect("run must launch");
    assert_eq!(
        out1.status.code().unwrap_or(-1),
        2,
        "--mount badNAME!:x must exit 2; stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // --mount @t/x:/abs/path → absolute target → exit 2.
    let out2 = lightr_cmd(home.path())
        .args(["run", "--mount", "@t/x:/abs/path", "--", "true"])
        .output()
        .expect("run must launch");
    assert_eq!(
        out2.status.code().unwrap_or(-1),
        2,
        "--mount @t/x:/abs/path must exit 2; stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // --mount @t/x:../escape → path escape → exit 2.
    let out3 = lightr_cmd(home.path())
        .args(["run", "--mount", "@t/x:../escape", "--", "true"])
        .output()
        .expect("run must launch");
    assert_eq!(
        out3.status.code().unwrap_or(-1),
        2,
        "--mount @t/x:../escape must exit 2; stderr: {}",
        String::from_utf8_lossy(&out3.stderr)
    );
}

// ---------------------------------------------------------------------------
// a12b — diff --dir
// ---------------------------------------------------------------------------
#[test]
fn a12b_diff_dir() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    fixture_tree(ws.path());

    // Snapshot @t/x.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // Modified copy → diff --dir <copy> --name @t/x exits 1, names the path.
    let modified_copy = TempDir::new().unwrap();
    // Copy fixture manually using fs operations.
    copy_dir_all(ws.path(), modified_copy.path());
    let changed_file = modified_copy.path().join("level1/sub1/deep1/file_0000.txt");
    fs::write(&changed_file, b"a12b modified content").unwrap();

    let diff_mod = lightr_cmd(home.path())
        .args([
            "diff",
            "--dir",
            modified_copy.path().to_str().unwrap(),
            "--name",
            "@t/x",
        ])
        .output()
        .expect("diff --dir must launch");
    assert_eq!(
        diff_mod.status.code().unwrap_or(-1),
        1,
        "diff --dir (modified copy) must exit 1; stderr: {}",
        String::from_utf8_lossy(&diff_mod.stderr)
    );
    let diff_stdout = String::from_utf8_lossy(&diff_mod.stdout);
    assert!(
        diff_stdout.contains("file_0000.txt"),
        "diff --dir must name the changed path; got: {diff_stdout}"
    );

    // Unmodified copy → exit 0.
    let clean_copy = TempDir::new().unwrap();
    copy_dir_all(ws.path(), clean_copy.path());

    lightr_cmd(home.path())
        .args([
            "diff",
            "--dir",
            clean_copy.path().to_str().unwrap(),
            "--name",
            "@t/x",
        ])
        .assert()
        .code(0);
}

// ---------------------------------------------------------------------------
// a9d — detach never populates the AC (plain run must be memo MISS)
// ---------------------------------------------------------------------------
#[test]
fn a9d_detach_no_memo() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    fixture_tree(ws.path());

    // Detach an echo command.
    let det_out = lightr_cmd(home.path())
        .args([
            "run",
            "-d",
            "--dir",
            ws.path().to_str().unwrap(),
            "--",
            "/bin/echo",
            "detach-memo-test",
        ])
        .output()
        .expect("run -d must launch");
    assert_eq!(
        det_out.status.code().unwrap_or(-1),
        0,
        "run -d must exit 0; stderr: {}",
        String::from_utf8_lossy(&det_out.stderr)
    );
    let det_id = parse_id_from_stdout(&det_out.stdout);
    let _guard = RunGuard::new(&det_id, home.path());

    // Wait for the detached run to exit.
    let became_exited = poll_until(Duration::from_secs(5), || {
        ps_is_exited(home.path(), &det_id)
    });
    assert!(
        became_exited,
        "detached run {det_id} must show running=false within 5 s"
    );

    // Plain run of the same command → must be memo MISS (detached never populated AC).
    let run_out = lightr_cmd(home.path())
        .args([
            "run",
            "--dir",
            ws.path().to_str().unwrap(),
            "--",
            "/bin/echo",
            "detach-memo-test",
        ])
        .output()
        .expect("run must launch");
    assert_eq!(
        run_out.status.code().unwrap_or(-1),
        0,
        "plain run must exit 0; stderr: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    let run_stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(
        run_stderr.to_ascii_uppercase().contains("MISS"),
        "plain run after detached run must be memo MISS; stderr: {run_stderr}"
    );
}
