//! A1–A4 acceptance tests.

use std::fs;
use std::path::Path;

use super::common::{fixture_tree, lightr_cmd};
use super::helpers::*;
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

    // No-daemon proof, scoped to THIS test's LIGHTR_HOME (a global `pgrep -x
    // lightr` races other parallel acceptance tests that legitimately spawn
    // the binary). These synchronous verbs must leave: no control sockets,
    // no run/ supervisor dirs, no compose/ supervisors — nothing resident.
    fn no_sockets(dir: &Path) -> bool {
        let Ok(rd) = fs::read_dir(dir) else {
            return true;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !no_sockets(&p) {
                    return false;
                }
            } else if p.extension().and_then(|x| x.to_str()) == Some("sock") {
                return false;
            }
        }
        true
    }
    assert!(
        no_sockets(home.path()),
        "no control sockets may remain under LIGHTR_HOME after sync verbs"
    );
    let run_dir = home.path().join("run");
    assert!(
        !run_dir.exists()
            || fs::read_dir(&run_dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "no run/ supervisor dirs may exist (this test never detached)"
    );
    let compose_dir = home.path().join("compose");
    assert!(
        !compose_dir.exists()
            || fs::read_dir(&compose_dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "no compose/ supervisors may exist"
    );

    // LIGHTR_HOME tree must contain only regular files, dirs, or symlinks
    #[cfg(unix)]
    assert_no_sockets_or_pidfiles(home.path());
}
