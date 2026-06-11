//! A1–A8 per build-spec v2 §8 — authored by WP-6.
//! Every test: LIGHTR_HOME → per-test tempdir; never touches ~.
//!
//! Gate: cargo check -p lightr-acceptance --all-targets must pass.
//! The binary is expected to have todo!() bodies (red-first suite).
//! Do NOT weaken assertions to make them pass against stubs.

#[path = "common/mod.rs"]
mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{fixture_tree, lightr_cmd};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// A1 — Roundtrip: snapshot → hydrate → recursive compare
// ---------------------------------------------------------------------------
#[test]
fn a1_roundtrip() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // snapshot
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/ws"])
        .assert()
        .success();

    // hydrate into fresh dir
    lightr_cmd(home.path())
        .args(["hydrate", dest.path().to_str().unwrap(), "--name", "@t/ws"])
        .assert()
        .success();

    // recursive compare: bytes, modes, symlink targets, empty dirs
    compare_trees(ws.path(), dest.path());
}

/// Walk `expected` recursively and assert `actual` matches byte-for-byte,
/// with identical st_mode & 0o777, symlink targets, and empty dirs.
fn compare_trees(expected: &Path, actual: &Path) {
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
            // empty dir: check that it stays empty on both sides
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
            // regular file: bytes + mode
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

/// Sorted DFS walk of all entries under `root` (dirs included for empty-dir checks).
fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    walk_recurse(root, &mut out);
    out.sort();
    out
}

fn walk_recurse(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd {
        let entry = entry.unwrap();
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        out.push(path.clone());
        if meta.file_type().is_dir() {
            walk_recurse(&path, out);
        }
    }
}

// ---------------------------------------------------------------------------
// A2 — Memo hit: second run is a HIT; side-effect file has exactly 1 line
// ---------------------------------------------------------------------------
#[test]
fn a2_memo_hit() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let side = TempDir::new().unwrap();

    fixture_tree(ws.path());

    let sidefx = side.path().join("sidefx");

    let run_args = |home_path: &Path| -> (Vec<u8>, Vec<u8>) {
        // command: echo $$ >> sidefx; echo out
        let cmd_str = format!("echo $$ >> {}; echo out", sidefx.to_str().unwrap());
        let out = lightr_cmd(home_path)
            .current_dir(ws.path())
            .args([
                "run",
                "--dir",
                ".",
                "--input",
                ws.path().to_str().unwrap(),
                "--",
                "/bin/sh",
                "-c",
                &cmd_str,
            ])
            .assert()
            .success()
            .get_output()
            .clone();
        (out.stdout, out.stderr)
    };

    let (stdout1, stderr1) = run_args(home.path());
    let (stdout2, stderr2) = run_args(home.path());

    // side-effect file must have exactly 1 line (second run was a HIT, no exec)
    let side_content =
        fs::read_to_string(&sidefx).unwrap_or_else(|_| panic!("sidefx file not created"));
    let lines: Vec<&str> = side_content.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected 1 line in sidefx (memo HIT suppressed second exec), got {}",
        lines.len()
    );

    // stderr markers
    let s1 = String::from_utf8_lossy(&stderr1);
    let s2 = String::from_utf8_lossy(&stderr2);
    assert!(
        s1.contains("memo MISS"),
        "first run stderr must contain 'memo MISS', got: {s1}"
    );
    assert!(
        s2.contains("memo HIT"),
        "second run stderr must contain 'memo HIT', got: {s2}"
    );

    // stdouts identical
    assert_eq!(
        stdout1, stdout2,
        "stdouts must be identical on memo HIT replay"
    );
}

// ---------------------------------------------------------------------------
// A3 — Failure not memoized: exit-7 cmd runs twice; both MISS; 2 side-effect lines
// ---------------------------------------------------------------------------
#[test]
fn a3_failure_not_memoized() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let side = TempDir::new().unwrap();

    fixture_tree(ws.path());

    let sidefx = side.path().join("sidefx");

    let run_once = |home_path: &Path| -> (i32, Vec<u8>) {
        let cmd_str = format!("echo x >> {}; exit 7", sidefx.to_str().unwrap());
        let output = lightr_cmd(home_path)
            .current_dir(ws.path())
            .args([
                "run",
                "--dir",
                ".",
                "--input",
                ws.path().to_str().unwrap(),
                "--",
                "/bin/sh",
                "-c",
                &cmd_str,
            ])
            .output()
            .unwrap();
        let code = output.status.code().unwrap_or(-1);
        (code, output.stderr)
    };

    let (code1, stderr1) = run_once(home.path());
    let (code2, stderr2) = run_once(home.path());

    // both exit 7
    assert_eq!(code1, 7, "first run must exit 7");
    assert_eq!(code2, 7, "second run must exit 7");

    // side-effect file must have exactly 2 lines (both runs executed)
    let side_content =
        fs::read_to_string(&sidefx).unwrap_or_else(|_| panic!("sidefx file not created"));
    let lines: Vec<&str> = side_content.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected 2 lines in sidefx (failure not memoized, both runs executed), got {}",
        lines.len()
    );

    // both stderr say MISS (not memoized)
    let s1 = String::from_utf8_lossy(&stderr1);
    let s2 = String::from_utf8_lossy(&stderr2);
    assert!(
        s1.contains("memo MISS"),
        "first run stderr must contain 'memo MISS', got: {s1}"
    );
    assert!(
        s2.contains("memo MISS"),
        "second run stderr must contain 'memo MISS', got: {s2}"
    );
}

// ---------------------------------------------------------------------------
// A4 — No daemon: pgrep -x lightr returns empty; LIGHTR_HOME has no sockets/pidfiles
// ---------------------------------------------------------------------------
#[test]
fn a4_no_daemon() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    let side = TempDir::new().unwrap();

    fixture_tree(ws.path());

    // Do a representative set of operations
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "@t/ws"])
        .assert()
        .success();

    let dest = TempDir::new().unwrap();
    lightr_cmd(home.path())
        .args(["hydrate", dest.path().to_str().unwrap(), "--name", "@t/ws"])
        .assert()
        .success();

    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["status", "--dir", ".", "--name", "@t/ws"])
        .assert()
        .success();

    let sidefx = side.path().join("sidefx");
    let cmd_str = format!("echo $$ >> {}; echo out", sidefx.to_str().unwrap());
    lightr_cmd(home.path())
        .current_dir(ws.path())
        .args([
            "run",
            "--dir",
            ".",
            "--input",
            ws.path().to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            &cmd_str,
        ])
        .assert()
        .success();

    // pgrep -x lightr must return non-zero (no process found)
    let pgrep = std::process::Command::new("pgrep")
        .args(["-x", "lightr"])
        .output()
        .expect("pgrep must be available");
    // pgrep exits 1 when no processes found
    assert!(
        !pgrep.status.success() || pgrep.stdout.trim_ascii().is_empty(),
        "pgrep -x lightr should find nothing; found: {}",
        String::from_utf8_lossy(&pgrep.stdout)
    );

    // LIGHTR_HOME tree must contain only regular files, dirs, or symlinks
    assert_no_sockets_or_pidfiles(home.path());
}

fn assert_no_sockets_or_pidfiles(root: &Path) {
    use std::os::unix::fs::FileTypeExt;

    let entries = walkdir(root);
    for path in &entries {
        let meta = fs::symlink_metadata(path).unwrap();
        let ft = meta.file_type();
        assert!(
            !ft.is_socket(),
            "found unexpected socket in LIGHTR_HOME: {}",
            path.display()
        );
        assert!(
            !ft.is_fifo(),
            "found unexpected FIFO in LIGHTR_HOME: {}",
            path.display()
        );
        // pidfiles: no *.pid files
        if let Some(name) = path.file_name() {
            let name = name.to_string_lossy();
            assert!(
                !name.ends_with(".pid"),
                "found pidfile in LIGHTR_HOME: {}",
                path.display()
            );
        }
    }
}

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
    use std::io::Write as _;
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

/// Walk `objects_root` and return the path to one regular file (any).
fn find_object_file(
    objects_root: &Path,
    pred: impl Fn(&[u8]) -> bool + Copy,
) -> Option<std::path::PathBuf> {
    fn recurse(dir: &Path, pred: &(impl Fn(&[u8]) -> bool + Copy)) -> Option<std::path::PathBuf> {
        let rd = fs::read_dir(dir).ok()?;
        for entry in rd {
            let entry = entry.ok()?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).ok()?;
            if meta.file_type().is_file() {
                if let Ok(bytes) = fs::read(&path) {
                    if !bytes.is_empty() && pred(&bytes) {
                        return Some(path);
                    }
                }
            } else if meta.file_type().is_dir() {
                if let Some(found) = recurse(&path, pred) {
                    return Some(found);
                }
            }
        }
        None
    }
    recurse(objects_root, &pred)
}

/// chmod writable, flip the first byte, reseal to 0o444 (spec: evidence kept).
fn corrupt_in_place(object_file: &Path) {
    let mut content = fs::read(object_file).unwrap();
    assert!(!content.is_empty(), "object file must not be empty");
    fs::set_permissions(object_file, fs::Permissions::from_mode(0o644)).unwrap();
    content[0] ^= 0xFF;
    fs::write(object_file, &content).unwrap();
    fs::set_permissions(object_file, fs::Permissions::from_mode(0o444)).unwrap();
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
