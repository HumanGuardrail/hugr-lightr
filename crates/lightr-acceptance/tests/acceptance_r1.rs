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
    // The boundary index must be 1.
    assert!(
        bisect_stdout.contains('1'),
        "bisect stdout must contain boundary index 1; got: {bisect_stdout}"
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
    stdin.flush().unwrap();

    // --- Read responses (id=1, id=2, id=3) ---
    let reader = std::io::BufReader::new(stdout);
    let mut responses: Vec<serde_json::Value> = Vec::new();
    let read_deadline = Instant::now() + Duration::from_secs(5);

    // We need 3 id-bearing responses (skip notifications from server if any).
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
            if responses.len() == 3 {
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

    // We must have received 3 id-bearing responses.
    assert_eq!(
        responses.len(),
        3,
        "mcp: expected 3 id-bearing responses; got {}",
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
}

// ---------------------------------------------------------------------------
// Shared tree comparison helper (reused from A11).
// ---------------------------------------------------------------------------
fn compare_trees(expected: &Path, actual: &Path) {
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
