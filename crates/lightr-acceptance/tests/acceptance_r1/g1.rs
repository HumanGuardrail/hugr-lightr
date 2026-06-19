use super::common::*;
use super::helpers::*;

use std::fs;
use std::time::Duration;

use tempfile::TempDir;

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
    let junk_ref_files: Vec<std::path::PathBuf> = files_after
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
            rel.starts_with(std::path::Path::new("objects"))
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

    // Sleep 2 s so the run dir's mtime is at least 2 whole seconds old.
    // gc uses integer-second arithmetic: age_secs = now_secs - mtime_secs.
    // The condition to remove is `age_secs > min_age_secs` (i.e. > 0 when
    // min_age=0), so age_secs must be ≥ 1.  The run dir's mtime may be
    // refreshed by the supervisor writing cleanup files (e.g. "status") up to
    // ~100 ms after ps_is_exited returns.  Sleeping 1 s would make age_secs
    // land on exactly 0 or 1 depending on OS scheduling jitter; 2 s guarantees
    // age_secs ≥ 1 regardless of when the last write to the run dir occurred.
    std::thread::sleep(Duration::from_secs(2));

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
