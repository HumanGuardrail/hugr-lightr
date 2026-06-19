use super::common::*;
use super::helpers::*;

use std::fs;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// A12 — undo / reflog
// ---------------------------------------------------------------------------
#[test]
fn a12_undo_reflog() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // Snapshot v1.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // Modify one file (a file guaranteed to exist from fixture_tree).
    let modified = ws.path().join("level1/sub1/deep1/file_0000.txt");
    fs::write(&modified, b"v2 content -- modified for a12").unwrap();

    // Snapshot v2.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // Restore original v1 bytes for the modified file (we need them for comparison).
    let v1_bytes = "x".repeat(1024);

    // undo --name @t/x → exit 0.
    lightr_cmd(home.path())
        .args(["undo", "--name", "@t/x"])
        .assert()
        .code(0);

    // Hydrate after undo → must produce v1 bytes for the modified file.
    let dest = TempDir::new().unwrap();
    lightr_cmd(home.path())
        .args(["hydrate", dest.path().to_str().unwrap(), "--name", "@t/x"])
        .assert()
        .success();

    let hydrated_file = dest.path().join("level1/sub1/deep1/file_0000.txt");
    let hydrated_bytes = fs::read(&hydrated_file)
        .unwrap_or_else(|_| panic!("hydrated file must exist: {}", hydrated_file.display()));
    assert_eq!(
        hydrated_bytes,
        v1_bytes.as_bytes(),
        "after undo, hydrated file must contain v1 bytes"
    );

    // diff --name @t/x --at 1 → exit 1 (different) and stdout names the modified path with '~'.
    // After undo: ref@{0}=v1, ref@{1}=v2 → diff shows the change.
    let diff_out = lightr_cmd(home.path())
        .args(["diff", "--name", "@t/x", "--at", "1"])
        .output()
        .expect("diff must launch");
    assert_eq!(
        diff_out.status.code().unwrap_or(-1),
        1,
        "diff --name @t/x --at 1 must exit 1 (different); stderr: {}",
        String::from_utf8_lossy(&diff_out.stderr)
    );
    let diff_stdout = String::from_utf8_lossy(&diff_out.stdout);
    assert!(
        diff_stdout.contains("file_0000.txt"),
        "diff stdout must name the modified file; got: {diff_stdout}"
    );
    assert!(
        diff_stdout.contains('~'),
        "diff stdout must contain '~' marker; got: {diff_stdout}"
    );

    // diff --name @t/nope → exit 2 (not found).
    lightr_cmd(home.path())
        .args(["diff", "--name", "@t/nope"])
        .assert()
        .code(2);
}

// ---------------------------------------------------------------------------
// A13 — bisect (amendment: no memo HIT assertion)
// ---------------------------------------------------------------------------
#[test]
fn a13_bisect() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();

    // Build the tree: start without the marker.
    fixture_tree(ws.path());

    // Snapshot idx3 (oldest) — good, no bad.marker.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // Snapshot idx2 — good, no bad.marker.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // Add bad.marker — snapshots from here on are "bad".
    fs::write(ws.path().join("bad.marker"), b"bad").unwrap();

    // Snapshot idx1 — bad (has bad.marker).
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // Snapshot idx0 (newest) — bad (has bad.marker).
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/x"])
        .assert()
        .success();

    // ref_log ordering: idx0=newest(bad), idx1=bad, idx2=good, idx3=oldest(good).
    // bisect: command exits 0 when file does NOT exist → "good".
    // Expected boundary: largest bad index adjacent to first good = idx1.
    let bisect_out = lightr_cmd(home.path())
        .args([
            "bisect",
            "--name",
            "@t/x",
            "--",
            "/bin/sh",
            "-c",
            "test ! -f bad.marker",
        ])
        .output()
        .expect("bisect must launch");
    assert_eq!(
        bisect_out.status.code().unwrap_or(-1),
        0,
        "bisect must exit 0 (found); stderr: {}",
        String::from_utf8_lossy(&bisect_out.stderr)
    );

    let bisect_stdout = String::from_utf8_lossy(&bisect_out.stdout);
    // Parse `index=<N>` from stdout and assert N == 1.
    // Format: "index=<N> root=<hash>\n"
    let index_val: u64 = bisect_stdout
        .lines()
        .find_map(|line| {
            // Split on whitespace tokens, find one starting with "index="
            line.split_whitespace()
                .find_map(|tok| tok.strip_prefix("index=").and_then(|n| n.parse().ok()))
        })
        .unwrap_or_else(|| panic!("bisect stdout must contain 'index=<N>'; got: {bisect_stdout}"));
    assert_eq!(
        index_val, 1,
        "bisect must report index=1 (first bad version); got index={index_val}; stdout: {bisect_stdout}"
    );
}

// ---------------------------------------------------------------------------
// A14 — plan
// ---------------------------------------------------------------------------
#[test]
fn a14_plan() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // plan snapshot --dir . --name @t/p → exit 0, prints counts.
    let plan_snap = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["plan", "snapshot", "--dir", ".", "--name", "@t/p"])
        .output()
        .expect("plan snapshot must launch");
    assert_eq!(
        plan_snap.status.code().unwrap_or(-1),
        0,
        "plan snapshot must exit 0; stderr: {}",
        String::from_utf8_lossy(&plan_snap.stderr)
    );
    // Must print file/byte counts.
    let plan_snap_stdout = String::from_utf8_lossy(&plan_snap.stdout);
    assert!(
        plan_snap_stdout.contains("files") || plan_snap_stdout.contains("bytes"),
        "plan snapshot must print file/byte counts; got: {plan_snap_stdout}"
    );

    // Object count UNCHANGED after plan snapshot (read-only).
    let objects_root = home.path().join("store/objects");
    let obj_count_before = count_files_under(&objects_root);
    // Run plan snapshot again to confirm no side effects.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["plan", "snapshot", "--dir", ".", "--name", "@t/p"])
        .assert()
        .code(0);
    let obj_count_after = count_files_under(&objects_root);
    assert_eq!(
        obj_count_before, obj_count_after,
        "plan snapshot must not ingest objects (object count must be unchanged)"
    );

    // Object count UNCHANGED after plan hydrate (read-only).
    let hydrate_dest = TempDir::new().unwrap();
    // snapshot first so there is something to hydrate
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/p"])
        .assert()
        .success();
    let obj_before_hydrate = count_files_under(&objects_root);
    lightr_cmd(home.path())
        .args([
            "plan",
            "hydrate",
            hydrate_dest.path().to_str().unwrap(),
            "--name",
            "@t/p",
        ])
        .assert()
        .code(0);
    let obj_after_hydrate = count_files_under(&objects_root);
    assert_eq!(
        obj_before_hydrate, obj_after_hydrate,
        "plan hydrate must not ingest objects (object count must be unchanged)"
    );

    // plan run --dir . -- /bin/echo hi → prints predict=MISS.
    let plan_run_miss = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["plan", "run", "--dir", ".", "--", "/bin/echo", "hi"])
        .output()
        .expect("plan run must launch");
    assert_eq!(
        plan_run_miss.status.code().unwrap_or(-1),
        0,
        "plan run (MISS) must exit 0; stderr: {}",
        String::from_utf8_lossy(&plan_run_miss.stderr)
    );
    let plan_run_miss_stdout = String::from_utf8_lossy(&plan_run_miss.stdout);
    assert!(
        plan_run_miss_stdout.to_ascii_uppercase().contains("MISS"),
        "plan run must predict MISS before any real run; got: {plan_run_miss_stdout}"
    );

    // Object count UNCHANGED after plan run (read-only).
    let obj_before_plan_run = count_files_under(&objects_root);
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["plan", "run", "--dir", ".", "--", "/bin/echo", "hi"])
        .assert()
        .code(0);
    let obj_after_plan_run = count_files_under(&objects_root);
    assert_eq!(
        obj_before_plan_run, obj_after_plan_run,
        "plan run must not ingest objects (object count must be unchanged)"
    );

    // Real run.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["run", "--dir", ".", "--", "/bin/echo", "hi"])
        .assert()
        .success();

    // plan run again → predict=HIT.
    let plan_run_hit = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["plan", "run", "--dir", ".", "--", "/bin/echo", "hi"])
        .output()
        .expect("plan run (HIT) must launch");
    assert_eq!(
        plan_run_hit.status.code().unwrap_or(-1),
        0,
        "plan run (HIT) must exit 0; stderr: {}",
        String::from_utf8_lossy(&plan_run_hit.stderr)
    );
    let plan_run_hit_stdout = String::from_utf8_lossy(&plan_run_hit.stdout);
    assert!(
        plan_run_hit_stdout.to_ascii_uppercase().contains("HIT"),
        "plan run must predict HIT after real run; got: {plan_run_hit_stdout}"
    );
}
