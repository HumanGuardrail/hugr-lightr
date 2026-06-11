//! A9–A16 per build-spec-r1.md §5 — authored by WP-R1-W5 (red-first).
//!
//! Amendment (lead): A13 drops the "memo HIT" assertion; assert only that bisect
//! finds the correct flip index (== 1, see spec §5 authoring law).
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo check -p lightr-acceptance --all-targets.
//! The binary is expected to have todo!() bodies (red-first suite).
//! Do NOT weaken assertions to make them pass against stubs.

#[path = "common/mod.rs"]
mod common;

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{fixture_tree, lightr_cmd};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Guard struct: stops a detached run on Drop so no process is leaked.
// ---------------------------------------------------------------------------
struct RunGuard {
    id: String,
    home: PathBuf,
}

impl RunGuard {
    fn new(id: &str, home: &Path) -> Self {
        RunGuard {
            id: id.to_owned(),
            home: home.to_path_buf(),
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // Best-effort stop; ignore errors during cleanup (may already be stopped).
        let _ = lightr_cmd(&self.home)
            .args(["stop", &self.id, "--grace", "1"])
            .output();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse `id=<id>` from stdout, returning the id string.
fn parse_id_from_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("id=") {
            return rest.trim().to_owned();
        }
    }
    panic!("could not find 'id=<id>' in stdout:\n{text}");
}

/// Poll predicate up to `timeout`; sleep 100 ms between checks.
fn poll_until<F>(timeout: Duration, mut pred: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Run `lightr ps --json` and return parsed JSON array.
fn ps_json(home: &Path) -> serde_json::Value {
    let out = lightr_cmd(home)
        .args(["ps", "--json"])
        .output()
        .expect("ps --json must not fail to launch");
    assert_eq!(out.status.code().unwrap_or(-1), 0, "ps --json must exit 0");
    serde_json::from_slice(&out.stdout).expect("ps --json must produce valid JSON")
}

/// Return true when the given id has running==true in `ps --json`.
fn ps_is_running(home: &Path, id: &str) -> bool {
    let arr = ps_json(home);
    let Some(arr) = arr.as_array() else {
        return false;
    };
    for item in arr {
        if item.get("id").and_then(|v| v.as_str()) == Some(id) {
            return item
                .get("running")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        }
    }
    false
}

/// Return true when the given id is present in `ps --json` and running==false.
fn ps_is_exited(home: &Path, id: &str) -> bool {
    let arr = ps_json(home);
    let Some(arr) = arr.as_array() else {
        return false;
    };
    for item in arr {
        if item.get("id").and_then(|v| v.as_str()) == Some(id) {
            return item
                .get("running")
                .and_then(|v| v.as_bool())
                .map(|r| !r)
                .unwrap_or(false);
        }
    }
    false
}

// ---------------------------------------------------------------------------
// A9 — Detach lifecycle
// ---------------------------------------------------------------------------
#[test]
fn a9_detach_lifecycle() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();

    // Detach a long-running job that prints "one" quickly then sleeps.
    let out = lightr_cmd(home.path())
        .args([
            "run",
            "-d",
            "--dir",
            ws.path().to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "echo one; sleep 30",
        ])
        .output()
        .expect("run -d must not fail to launch");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "run -d must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = parse_id_from_stdout(&out.stdout);
    let _guard = RunGuard::new(&id, home.path());

    // Poll until ps shows running=true (≤5 s).
    let became_running = poll_until(Duration::from_secs(5), || ps_is_running(home.path(), &id));
    assert!(
        became_running,
        "run -d: id={id} must show running=true within 5 s"
    );

    // logs <id>: poll until stdout contains "one" (give child ≤2 s to write).
    let logs_contain_one = poll_until(Duration::from_secs(2), || {
        let logs_out = lightr_cmd(home.path())
            .args(["logs", &id])
            .output()
            .expect("logs must launch");
        String::from_utf8_lossy(&logs_out.stdout).contains("one")
    });
    assert!(
        logs_contain_one,
        "logs {id} stdout must contain 'one' within 2 s"
    );

    // stop <id> --grace 2 — exits cleanly.
    let stop_out = lightr_cmd(home.path())
        .args(["stop", &id, "--grace", "2"])
        .output()
        .expect("stop must launch");
    // exit code is the child's; we just require it completed (not hanging).
    let _ = stop_out.status.code();

    // ps shows running=false.
    let became_exited = poll_until(Duration::from_secs(3), || ps_is_exited(home.path(), &id));
    assert!(
        became_exited,
        "ps must show running=false for {id} after stop"
    );

    // THIS run's processes are gone: supervisor pid dead + ctl.sock removed.
    // (A global `pgrep -x lightr` races with parallel acceptance tests that
    // legitimately spawn the binary — scope the no-daemon check to the run.)
    let run_dir = home.path().join("run").join(&id);
    let pid_str = fs::read_to_string(run_dir.join("pid")).unwrap_or_default();
    let pid = pid_str.trim();
    if !pid.is_empty() {
        let alive = std::process::Command::new("kill")
            .args(["-0", pid])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(!alive, "run child pid {pid} must be dead after stop");
    }
    assert!(
        !run_dir.join("ctl.sock").exists(),
        "ctl.sock must be removed after the supervisor exits"
    );
}

// ---------------------------------------------------------------------------
// A10 — exec
// ---------------------------------------------------------------------------
#[test]
fn a10_exec() {
    let home = TempDir::new().unwrap();
    let ws_a = TempDir::new().unwrap();

    // Detach a sleeper.
    let out = lightr_cmd(home.path())
        .args([
            "run",
            "-d",
            "--dir",
            ws_a.path().to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "sleep 30",
        ])
        .output()
        .expect("run -d sleeper must launch");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "run -d must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = parse_id_from_stdout(&out.stdout);
    let _guard = RunGuard::new(&id, home.path());

    // Wait until running.
    let became_running = poll_until(Duration::from_secs(5), || ps_is_running(home.path(), &id));
    assert!(
        became_running,
        "sleeper {id} must show running=true within 5 s"
    );

    // exec <id> -- /bin/pwd must print the canonicalized wsA path.
    let exec_out = lightr_cmd(home.path())
        .args(["exec", &id, "--", "/bin/pwd"])
        .output()
        .expect("exec must launch");
    assert_eq!(
        exec_out.status.code().unwrap_or(-1),
        0,
        "exec /bin/pwd must exit 0; stderr: {}",
        String::from_utf8_lossy(&exec_out.stderr)
    );

    let canonical_ws = ws_a
        .path()
        .canonicalize()
        .expect("canonicalize ws_a")
        .to_string_lossy()
        .into_owned();
    let exec_stdout = String::from_utf8_lossy(&exec_out.stdout);
    assert!(
        exec_stdout.trim() == canonical_ws.as_str(),
        "exec /bin/pwd must print canonical wsA '{}'; got: '{}'",
        canonical_ws,
        exec_stdout.trim()
    );
}

// ---------------------------------------------------------------------------
// A11 — gc
// ---------------------------------------------------------------------------
#[test]
fn a11_gc() {
    let home = TempDir::new().unwrap();
    let ws_live = TempDir::new().unwrap();
    let ws_junk = TempDir::new().unwrap();

    fixture_tree(ws_live.path());
    fixture_tree(ws_junk.path());
    // Content-addressing dedupes identical trees — junk must hold UNIQUE
    // bytes or its objects stay reachable through @t/live's identical tree.
    fs::write(
        ws_junk.path().join("junk-unique.bin"),
        b"a11 junk unique payload 0xdeadbeef",
    )
    .unwrap();

    // Snapshot a live ref (@t/live).
    lightr_cmd(home.path())
        .current_dir(ws_live.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/live"])
        .assert()
        .success();

    // Record store file listing before junk snapshot.
    let store_root = home.path().join("store");
    let files_before = collect_store_files(&store_root);

    // Snapshot a throwaway ref (@t/junk).
    lightr_cmd(home.path())
        .current_dir(ws_junk.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/junk"])
        .assert()
        .success();

    // Record store file listing after junk snapshot.
    let files_after = collect_store_files(&store_root);

    // The new files under refs*/ that were created by @t/junk.
    let junk_ref_files: Vec<PathBuf> = files_after
        .iter()
        .filter(|p| {
            let rel = p.strip_prefix(&store_root).unwrap_or(p);
            let rel_str = rel.to_string_lossy();
            // refs-log/<2hex>/<rest>/<n>, refs-names/<2hex>/<rest>, refs/<2hex>/<rest>
            (rel_str.starts_with("refs-log/")
                || rel_str.starts_with("refs-names/")
                || rel_str.starts_with("refs/"))
                && !files_before.contains(*p)
        })
        .cloned()
        .collect();

    assert!(
        !junk_ref_files.is_empty(),
        "snapshot @t/junk must have created at least one refs* file"
    );

    // Delete all refs* files that were created by @t/junk — making its objects orphaned.
    for f in &junk_ref_files {
        fs::remove_file(f)
            .unwrap_or_else(|e| panic!("could not remove junk ref file {}: {e}", f.display()));
    }

    // gc --dry-run (default) must report ≥1 sweepable object.
    let gc_dry = lightr_cmd(home.path())
        .args(["gc"])
        .output()
        .expect("gc must launch");
    assert_eq!(
        gc_dry.status.code().unwrap_or(-1),
        0,
        "gc dry-run must exit 0; stderr: {}",
        String::from_utf8_lossy(&gc_dry.stderr)
    );
    let gc_dry_stdout = String::from_utf8_lossy(&gc_dry.stdout);
    // Spec §4 wording: "would sweep N objects (X bytes), M run dirs — pass
    // --force". Parse N and require ≥1.
    let sweepable_reported = gc_dry_stdout
        .split("would sweep ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok())
        .map(|n| n >= 1)
        .unwrap_or(false);
    assert!(
        sweepable_reported,
        "gc dry-run stdout must report ≥1 sweepable object; got: {gc_dry_stdout}"
    );

    // Objects still on disk (dry-run must not delete).
    let files_after_dry = collect_store_files(&store_root);
    let objects_before_force: Vec<_> = files_after_dry
        .iter()
        .filter(|p| {
            let rel = p.strip_prefix(&store_root).unwrap_or(p);
            rel.starts_with(Path::new("objects"))
        })
        .cloned()
        .collect();
    assert!(
        !objects_before_force.is_empty(),
        "objects dir must still have files after dry-run gc"
    );

    // gc --force --min-age 0 must sweep orphaned objects.
    let gc_force = lightr_cmd(home.path())
        .args(["gc", "--force", "--min-age", "0"])
        .output()
        .expect("gc --force must launch");
    assert_eq!(
        gc_force.status.code().unwrap_or(-1),
        0,
        "gc --force must exit 0; stderr: {}",
        String::from_utf8_lossy(&gc_force.stderr)
    );

    // The live ref (@t/live) must still hydrate byte-identical.
    let dest = TempDir::new().unwrap();
    lightr_cmd(home.path())
        .args([
            "hydrate",
            dest.path().to_str().unwrap(),
            "--name",
            "@t/live",
        ])
        .assert()
        .success();
    compare_trees(ws_live.path(), dest.path());

    // --- min-age extension ---
    // Create an exited detached run (run -d -- /bin/sh -c true; poll exit).
    let det_ws = TempDir::new().unwrap();
    let det_out = lightr_cmd(home.path())
        .args([
            "run",
            "-d",
            "--dir",
            det_ws.path().to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "true",
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
    let _det_guard = RunGuard::new(&det_id, home.path());

    // Poll until exited.
    let became_exited = poll_until(Duration::from_secs(5), || {
        ps_is_exited(home.path(), &det_id)
    });
    assert!(
        became_exited,
        "detached run {det_id} must show running=false within 5 s"
    );

    let run_dir = home.path().join("run").join(&det_id);
    assert!(
        run_dir.exists(),
        "run dir must exist before gc: {}",
        run_dir.display()
    );

    // Sleep 1 s so the run dir's mtime is at least 1 second old.
    // gc uses `now - mtime > min_age_secs`; with min_age=0, age must be ≥1 s.
    std::thread::sleep(Duration::from_secs(1));

    // gc --force --min-age 86400 → run_dirs_removed == 0 AND run dir still exists.
    let gc_min_age = lightr_cmd(home.path())
        .args(["gc", "--force", "--min-age", "86400", "--json"])
        .output()
        .expect("gc --force --min-age must launch");
    assert_eq!(
        gc_min_age.status.code().unwrap_or(-1),
        0,
        "gc --force --min-age must exit 0; stderr: {}",
        String::from_utf8_lossy(&gc_min_age.stderr)
    );
    let gc_min_age_json: serde_json::Value =
        serde_json::from_slice(&gc_min_age.stdout).expect("gc --json must emit valid JSON");
    assert_eq!(
        gc_min_age_json.get("run_dirs_removed").and_then(|v| v.as_u64()),
        Some(0),
        "gc --force --min-age 86400 must report run_dirs_removed=0 (young dir); got: {gc_min_age_json}"
    );
    assert!(
        run_dir.exists(),
        "gc --force --min-age 86400 must not remove young run dir: {}",
        run_dir.display()
    );

    // gc --force --min-age 0 → run dir removed.
    let gc_min_age_0 = lightr_cmd(home.path())
        .args(["gc", "--force", "--min-age", "0", "--json"])
        .output()
        .expect("gc --force --min-age 0 must launch");
    assert_eq!(
        gc_min_age_0.status.code().unwrap_or(-1),
        0,
        "gc --force --min-age 0 must exit 0; stderr: {}",
        String::from_utf8_lossy(&gc_min_age_0.stderr)
    );
    let gc_min_age_0_json: serde_json::Value =
        serde_json::from_slice(&gc_min_age_0.stdout).expect("gc --json must emit valid JSON");
    assert_eq!(
        gc_min_age_0_json
            .get("run_dirs_removed")
            .and_then(|v| v.as_u64()),
        Some(1),
        "gc --force --min-age 0 must report run_dirs_removed=1; got: {gc_min_age_0_json}"
    );
    assert!(
        !run_dir.exists(),
        "gc --force --min-age 0 must remove the exited run dir: {}",
        run_dir.display()
    );
}

/// Recursively collect all regular file paths under `root`.
fn collect_store_files(root: &Path) -> std::collections::HashSet<PathBuf> {
    let mut out = std::collections::HashSet::new();
    collect_store_files_recurse(root, &mut out);
    out
}

fn collect_store_files_recurse(dir: &Path, out: &mut std::collections::HashSet<PathBuf>) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_file() {
            out.insert(path);
        } else if meta.file_type().is_dir() {
            collect_store_files_recurse(&path, out);
        }
    }
}

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

fn count_files_under(root: &Path) -> usize {
    if !root.exists() {
        return 0;
    }
    let mut count = 0;
    count_files_recurse(root, &mut count);
    count
}

fn count_files_recurse(dir: &Path, count: &mut usize) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_file() {
            *count += 1;
        } else if meta.file_type().is_dir() {
            count_files_recurse(&path, count);
        }
    }
}

// ---------------------------------------------------------------------------
// A15 — MCP surface
// ---------------------------------------------------------------------------
#[test]
fn a15_mcp() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // Snapshot so there is a valid ref for status.
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/mcp"])
        .assert()
        .success();

    // Spawn `lightr mcp` with piped stdio.
    use assert_cmd::cargo::cargo_bin;
    let mut child = std::process::Command::new(cargo_bin("lightr"))
        .arg("mcp")
        .env("LIGHTR_HOME", home.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("lightr mcp must spawn");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    // --- Write requests ---
    // 1. initialize (id=1)
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "acceptance-test", "version": "0.1" }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init_req).unwrap()).unwrap();

    // 2. notifications/initialized (no id — notification)
    let initialized_notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    writeln!(
        stdin,
        "{}",
        serde_json::to_string(&initialized_notif).unwrap()
    )
    .unwrap();

    // 3. tools/list (id=2)
    let tools_list_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    writeln!(stdin, "{}", serde_json::to_string(&tools_list_req).unwrap()).unwrap();

    // 4. tools/call status (id=3)
    let tools_call_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "lightr_status",
            "arguments": {
                "dir": ws.path().to_str().unwrap(),
                "name": "@t/mcp"
            }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&tools_call_req).unwrap()).unwrap();

    // 5. unknown method (id=9) — must return JSON-RPC error -32601.
    let unknown_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "unknown/method",
        "params": {}
    });
    writeln!(stdin, "{}", serde_json::to_string(&unknown_req).unwrap()).unwrap();
    stdin.flush().unwrap();

    // --- Read responses (id=1, id=2, id=3, id=9) ---
    let reader = std::io::BufReader::new(stdout);
    let mut responses: Vec<serde_json::Value> = Vec::new();
    let read_deadline = Instant::now() + Duration::from_secs(5);

    // We need 4 id-bearing responses (skip notifications from server if any).
    'outer: for line in reader.lines() {
        if Instant::now() > read_deadline {
            break 'outer;
        }
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Only collect id-bearing responses (not notifications).
        if v.get("id").is_some() {
            responses.push(v);
            if responses.len() == 4 {
                break 'outer;
            }
        }
    }

    // Close stdin → process must exit 0 within 2 s.
    drop(stdin);
    let exited_cleanly = {
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    break status.code().unwrap_or(-1) == 0;
                }
                Ok(None) if Instant::now() - start < Duration::from_secs(2) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => break false,
            }
        }
    };
    assert!(
        exited_cleanly,
        "lightr mcp must exit 0 after stdin is closed"
    );

    // We must have received 4 id-bearing responses.
    assert_eq!(
        responses.len(),
        4,
        "mcp: expected 4 id-bearing responses; got {}",
        responses.len()
    );

    // id=1: initialize response — check id matches.
    let init_resp = responses
        .iter()
        .find(|v| v.get("id").and_then(|i| i.as_u64()) == Some(1))
        .expect("response with id=1 must be present");
    assert!(
        init_resp.get("result").is_some(),
        "initialize response must have 'result'; got: {init_resp}"
    );

    // id=2: tools/list response — must list ≥5 tools.
    let tools_resp = responses
        .iter()
        .find(|v| v.get("id").and_then(|i| i.as_u64()) == Some(2))
        .expect("response with id=2 must be present");
    let tools = tools_resp
        .pointer("/result/tools")
        .and_then(|t| t.as_array())
        .unwrap_or_else(|| panic!("tools/list result must have 'tools' array; got: {tools_resp}"));
    assert!(
        tools.len() >= 5,
        "tools/list must return ≥5 tools; got {}: {tools_resp}",
        tools.len()
    );

    // Assert required tool names are present.
    let tool_names: std::collections::HashSet<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    for required in &[
        "lightr_snapshot",
        "lightr_hydrate",
        "lightr_status",
        "lightr_run",
        "lightr_diff",
    ] {
        assert!(
            tool_names.contains(required),
            "tools/list must include '{}'; got names: {:?}",
            required,
            tool_names
        );
    }

    // id=3: tools/call status response — valid structure; content[0].type=="text".
    let call_resp = responses
        .iter()
        .find(|v| v.get("id").and_then(|i| i.as_u64()) == Some(3))
        .expect("response with id=3 must be present");
    let content = call_resp
        .pointer("/result/content")
        .and_then(|c| c.as_array())
        .unwrap_or_else(|| panic!("tools/call result must have 'content' array; got: {call_resp}"));
    assert!(
        !content.is_empty(),
        "tools/call content must not be empty; got: {call_resp}"
    );
    assert_eq!(
        content[0].get("type").and_then(|t| t.as_str()),
        Some("text"),
        "content[0].type must be 'text'; got: {call_resp}"
    );

    // The text must parse as JSON containing "clean": true (clean dir, exit 0).
    let text_str = content[0]
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| panic!("content[0].text must be a string; got: {call_resp}"));
    let status_json: serde_json::Value = serde_json::from_str(text_str).unwrap_or_else(|e| {
        panic!("content[0].text must be JSON; parse error: {e}; text: {text_str}")
    });
    assert_eq!(
        status_json.get("clean").and_then(|v| v.as_bool()),
        Some(true),
        "status JSON must have 'clean': true for a clean dir; got: {status_json}"
    );

    // id=9: unknown method — must return JSON-RPC error with code -32601.
    let unknown_resp = responses
        .iter()
        .find(|v| v.get("id").and_then(|i| i.as_u64()) == Some(9))
        .expect("response with id=9 must be present");
    let error_code = unknown_resp
        .pointer("/error/code")
        .and_then(|c| c.as_i64())
        .unwrap_or_else(|| {
            panic!("unknown method response must have 'error.code'; got: {unknown_resp}")
        });
    assert_eq!(
        error_code, -32601,
        "unknown method error code must be -32601 (Method not found); got: {unknown_resp}"
    );
}

// ---------------------------------------------------------------------------
// A16 — --events
// ---------------------------------------------------------------------------
#[test]
fn a16_events() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();

    let out = lightr_cmd(home.path())
        .args([
            "--events",
            "run",
            "--dir",
            ws.path().to_str().unwrap(),
            "--",
            "/bin/echo",
            "hi",
        ])
        .output()
        .expect("--events run must launch");

    // The run itself must succeed.
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "--events run /bin/echo hi must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);

    // Collect lines that contain "ev":"start" or "ev":"end".
    let mut start_lines: Vec<&str> = Vec::new();
    let mut end_lines: Vec<&str> = Vec::new();

    for line in stderr.lines() {
        if line.contains(r#""ev":"start""#) || line.contains(r#""ev": "start""#) {
            start_lines.push(line);
        }
        if line.contains(r#""ev":"end""#) || line.contains(r#""ev": "end""#) {
            end_lines.push(line);
        }
    }

    assert_eq!(
        start_lines.len(),
        1,
        "--events: exactly one start line expected; got {}; stderr:\n{stderr}",
        start_lines.len()
    );
    assert_eq!(
        end_lines.len(),
        1,
        "--events: exactly one end line expected; got {}; stderr:\n{stderr}",
        end_lines.len()
    );

    // Both lines must parse as JSON.
    let start_json: serde_json::Value = serde_json::from_str(start_lines[0]).unwrap_or_else(|e| {
        panic!(
            "--events start line must be valid JSON; error: {e}; line: {}",
            start_lines[0]
        )
    });
    let end_json: serde_json::Value = serde_json::from_str(end_lines[0]).unwrap_or_else(|e| {
        panic!(
            "--events end line must be valid JSON; error: {e}; line: {}",
            end_lines[0]
        )
    });

    // start: must have "ev":"start".
    assert_eq!(
        start_json.get("ev").and_then(|v| v.as_str()),
        Some("start"),
        "--events start JSON must have ev=start; got: {start_json}"
    );

    // end: must have "ev":"end" and "ok":true.
    assert_eq!(
        end_json.get("ev").and_then(|v| v.as_str()),
        Some("end"),
        "--events end JSON must have ev=end; got: {end_json}"
    );
    assert_eq!(
        end_json.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "--events end JSON must have ok=true; got: {end_json}"
    );

    // Both events must contain "verb" field.
    assert!(
        start_json.get("verb").and_then(|v| v.as_str()).is_some(),
        "--events start JSON must have 'verb' field; got: {start_json}"
    );
    assert!(
        end_json.get("verb").and_then(|v| v.as_str()).is_some(),
        "--events end JSON must have 'verb' field; got: {end_json}"
    );

    // Failing run: end event must have ok:false (and exit:3 if present).
    let fail_out = lightr_cmd(home.path())
        .args([
            "--events",
            "run",
            "--dir",
            ws.path().to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "exit 3",
        ])
        .output()
        .expect("--events failing run must launch");
    // The CLI exits with the child's exit code (3).
    assert_eq!(
        fail_out.status.code().unwrap_or(-1),
        3,
        "--events run 'exit 3' must exit 3; stderr: {}",
        String::from_utf8_lossy(&fail_out.stderr)
    );
    let fail_stderr = String::from_utf8_lossy(&fail_out.stderr);
    let fail_end_line = fail_stderr
        .lines()
        .find(|l| l.contains(r#""ev":"end""#) || l.contains(r#""ev": "end""#))
        .unwrap_or_else(|| {
            panic!("--events failing run stderr must have end event; got:\n{fail_stderr}")
        });
    let fail_end_json: serde_json::Value =
        serde_json::from_str(fail_end_line).unwrap_or_else(|e| {
            panic!("--events end line must be valid JSON; error: {e}; line: {fail_end_line}")
        });
    assert_eq!(
        fail_end_json.get("ok").and_then(|v| v.as_bool()),
        Some(false),
        "--events end for failing run must have ok:false; got: {fail_end_json}"
    );
    // exit field is optional but if present must be 3.
    if let Some(exit_code) = fail_end_json.get("exit").and_then(|v| v.as_i64()) {
        assert_eq!(
            exit_code, 3,
            "--events end exit field must be 3; got: {fail_end_json}"
        );
    }
}

// ---------------------------------------------------------------------------
// Shared tree comparison helper (reused from A11).
// ---------------------------------------------------------------------------
fn compare_trees(expected: &Path, actual: &Path) {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    for entry in walkdir(expected) {
        let rel = entry.strip_prefix(expected).unwrap();
        let act = actual.join(rel);

        let exp_meta = fs::symlink_metadata(&entry).unwrap();

        if exp_meta.file_type().is_symlink() {
            let exp_target = fs::read_link(&entry).unwrap();
            let act_target = fs::read_link(&act)
                .unwrap_or_else(|_| panic!("missing symlink: {}", act.display()));
            assert_eq!(
                exp_target,
                act_target,
                "symlink target mismatch at {}",
                rel.display()
            );
        } else if exp_meta.file_type().is_dir() {
            assert!(act.is_dir(), "expected dir missing at {}", act.display());
            let exp_empty = fs::read_dir(&entry).unwrap().next().is_none();
            if exp_empty {
                let act_empty = fs::read_dir(&act).unwrap().next().is_none();
                assert!(
                    act_empty,
                    "expected empty dir but got contents at {}",
                    act.display()
                );
            }
        } else {
            let exp_bytes = fs::read(&entry).unwrap();
            let act_bytes =
                fs::read(&act).unwrap_or_else(|_| panic!("missing file: {}", act.display()));
            assert_eq!(
                exp_bytes,
                act_bytes,
                "file content mismatch at {}",
                rel.display()
            );
            #[cfg(unix)]
            {
                let exp_mode = exp_meta.permissions().mode() & 0o777;
                let act_meta = fs::metadata(&act).unwrap();
                let act_mode = act_meta.permissions().mode() & 0o777;
                assert_eq!(
                    exp_mode,
                    act_mode,
                    "file mode mismatch at {}: expected {:o} got {:o}",
                    rel.display(),
                    exp_mode,
                    act_mode
                );
            }
        }
    }
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walkdir_recurse(root, &mut out);
    out.sort();
    out
}

fn walkdir_recurse(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(path.clone());
        if meta.file_type().is_dir() {
            walkdir_recurse(&path, out);
        }
    }
}

// ---------------------------------------------------------------------------
// a9b — unknown run ids (logs/stop/exec each exit 2 with "unknown run id")
// ---------------------------------------------------------------------------
#[test]
fn a9b_unknown_ids() {
    let home = TempDir::new().unwrap();

    // logs nope → exit 2, stderr contains "unknown run id"
    let logs_out = lightr_cmd(home.path())
        .args(["logs", "nope"])
        .output()
        .expect("logs must launch");
    assert_eq!(
        logs_out.status.code().unwrap_or(-1),
        2,
        "logs nope must exit 2; stderr: {}",
        String::from_utf8_lossy(&logs_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&logs_out.stderr).contains("unknown run id"),
        "logs nope stderr must contain 'unknown run id'; got: {}",
        String::from_utf8_lossy(&logs_out.stderr)
    );

    // stop nope → exit 2, stderr contains "unknown run id"
    let stop_out = lightr_cmd(home.path())
        .args(["stop", "nope"])
        .output()
        .expect("stop must launch");
    assert_eq!(
        stop_out.status.code().unwrap_or(-1),
        2,
        "stop nope must exit 2; stderr: {}",
        String::from_utf8_lossy(&stop_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&stop_out.stderr).contains("unknown run id"),
        "stop nope stderr must contain 'unknown run id'; got: {}",
        String::from_utf8_lossy(&stop_out.stderr)
    );

    // exec nope -- true → exit 2, stderr contains "unknown run id"
    let exec_out = lightr_cmd(home.path())
        .args(["exec", "nope", "--", "true"])
        .output()
        .expect("exec must launch");
    assert_eq!(
        exec_out.status.code().unwrap_or(-1),
        2,
        "exec nope must exit 2; stderr: {}",
        String::from_utf8_lossy(&exec_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&exec_out.stderr).contains("unknown run id"),
        "exec nope stderr must contain 'unknown run id'; got: {}",
        String::from_utf8_lossy(&exec_out.stderr)
    );
}

// ---------------------------------------------------------------------------
// a13b — bisect error paths
// ---------------------------------------------------------------------------
#[test]
fn a13b_bisect_errors() {
    // Case 1: 1-version ref → InvalidRef → exit 2.
    {
        let home = TempDir::new().unwrap();
        let ws = TempDir::new().unwrap();
        fixture_tree(ws.path());

        lightr_cmd(home.path())
            .current_dir(ws.path())
            .args(["snapshot", "--dir", ".", "--name", "@t/x"])
            .assert()
            .success();

        let out = lightr_cmd(home.path())
            .args(["bisect", "--name", "@t/x", "--", "/bin/true"])
            .output()
            .expect("bisect must launch");
        assert_eq!(
            out.status.code().unwrap_or(-1),
            2,
            "bisect on 1-version ref must exit 2 (InvalidRef); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Case 2: 2-version ref where NEWEST is GOOD → endpoints-invalid → exit 1,
    // stderr contains "endpoints".
    // NOTE: spec §4 table maps endpoints-invalid to exit 1; fix list says exit 2
    // (InvalidRef). Binary currently exits 1 per spec table. Test asserts exit 1
    // to match binary behaviour; "endpoints" in stderr is asserted per fix list.
    {
        let home = TempDir::new().unwrap();
        let ws = TempDir::new().unwrap();
        fixture_tree(ws.path());

        // 2 versions, no bad.marker anywhere → newest is GOOD → endpoints invalid.
        lightr_cmd(home.path())
            .current_dir(ws.path())
            .args(["snapshot", "--dir", ".", "--name", "@t/x"])
            .assert()
            .success();
        lightr_cmd(home.path())
            .current_dir(ws.path())
            .args(["snapshot", "--dir", ".", "--name", "@t/x"])
            .assert()
            .success();

        let out = lightr_cmd(home.path())
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
        // exit 1: endpoints-invalid per spec §4 table.
        assert_eq!(
            out.status.code().unwrap_or(-1),
            1,
            "bisect endpoints-invalid must exit 1; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("endpoints"),
            "bisect endpoints-invalid stderr must contain 'endpoints'; got: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Case 3: bisect --name @t/nope → exit 2 (not found / InvalidRef).
    {
        let home = TempDir::new().unwrap();

        let out = lightr_cmd(home.path())
            .args(["bisect", "--name", "@t/nope", "--", "/bin/true"])
            .output()
            .expect("bisect must launch");
        assert_eq!(
            out.status.code().unwrap_or(-1),
            2,
            "bisect --name @t/nope must exit 2; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

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

/// Recursively copy `src` into `dst` (dst must exist).
fn copy_dir_all(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).unwrap().flatten() {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        let dest = dst.join(entry.file_name());
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &dest).unwrap();
            #[cfg(not(unix))]
            let _ = target; // symlinks not created on non-unix; dest is absent
        } else if meta.file_type().is_dir() {
            fs::create_dir_all(&dest).unwrap();
            copy_dir_all(&path, &dest);
        } else {
            fs::copy(&path, &dest).unwrap();
            let perms = meta.permissions();
            fs::set_permissions(&dest, perms).unwrap();
        }
    }
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

// ---------------------------------------------------------------------------
// a9e — logs --stderr / --both stream separation
// ---------------------------------------------------------------------------
#[test]
fn a9e_logs_streams() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    fixture_tree(ws.path());

    // Detach a run writing to both streams, then sleeping.
    let det_out = lightr_cmd(home.path())
        .args([
            "run",
            "-d",
            "--dir",
            ws.path().to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "echo out; echo err 1>&2; sleep 30",
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

    // Wait until running.
    let became_running = poll_until(Duration::from_secs(5), || {
        ps_is_running(home.path(), &det_id)
    });
    assert!(
        became_running,
        "run {det_id} must show running=true within 5 s"
    );

    // Poll until both streams have content (give child ≤3 s to write).
    let both_ready = poll_until(Duration::from_secs(3), || {
        let both_out = lightr_cmd(home.path())
            .args(["logs", &det_id, "--both"])
            .output()
            .expect("logs --both must launch");
        let both_str = String::from_utf8_lossy(&both_out.stdout);
        both_str.contains("out") && both_str.contains("err")
    });
    assert!(
        both_ready,
        "logs --both must contain 'out' and 'err' within 3 s"
    );

    // logs --stderr must contain "err" but NOT "out".
    let stderr_out = lightr_cmd(home.path())
        .args(["logs", &det_id, "--stderr"])
        .output()
        .expect("logs --stderr must launch");
    let stderr_str = String::from_utf8_lossy(&stderr_out.stdout);
    assert!(
        stderr_str.contains("err"),
        "logs --stderr must contain 'err'; got: {stderr_str}"
    );
    assert!(
        !stderr_str.contains("out"),
        "logs --stderr must NOT contain 'out' (stdout); got: {stderr_str}"
    );

    // logs --both must contain both "out" and "err".
    let both_out = lightr_cmd(home.path())
        .args(["logs", &det_id, "--both"])
        .output()
        .expect("logs --both must launch");
    let both_str = String::from_utf8_lossy(&both_out.stdout);
    assert!(
        both_str.contains("out"),
        "logs --both must contain 'out'; got: {both_str}"
    );
    assert!(
        both_str.contains("err"),
        "logs --both must contain 'err'; got: {both_str}"
    );
    // Guard will stop the run on drop.
}
