use super::common::*;

use tempfile::TempDir;

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
// a9b — unknown ids (logs/stop/exec each exit 1, "No such container" — Docker
// parity, WP-EXIT-CODE; was lightr's pre-parity exit 2 / "unknown run id")
// ---------------------------------------------------------------------------
#[test]
fn a9b_unknown_ids() {
    let home = TempDir::new().unwrap();

    // logs nope → exit 1, stderr contains "No such container" (Docker parity)
    let logs_out = lightr_cmd(home.path())
        .args(["logs", "nope"])
        .output()
        .expect("logs must launch");
    assert_eq!(
        logs_out.status.code().unwrap_or(-1),
        1,
        "logs nope must exit 1 (Docker parity); stderr: {}",
        String::from_utf8_lossy(&logs_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&logs_out.stderr).contains("No such container"),
        "logs nope stderr must contain 'No such container'; got: {}",
        String::from_utf8_lossy(&logs_out.stderr)
    );

    // stop nope → exit 1, stderr contains "No such container" (Docker parity)
    let stop_out = lightr_cmd(home.path())
        .args(["stop", "nope"])
        .output()
        .expect("stop must launch");
    assert_eq!(
        stop_out.status.code().unwrap_or(-1),
        1,
        "stop nope must exit 1 (Docker parity); stderr: {}",
        String::from_utf8_lossy(&stop_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&stop_out.stderr).contains("No such container"),
        "stop nope stderr must contain 'No such container'; got: {}",
        String::from_utf8_lossy(&stop_out.stderr)
    );

    // exec nope -- true → exit 1, stderr contains "No such container" (Docker parity)
    let exec_out = lightr_cmd(home.path())
        .args(["exec", "nope", "--", "true"])
        .output()
        .expect("exec must launch");
    assert_eq!(
        exec_out.status.code().unwrap_or(-1),
        1,
        "exec nope must exit 1 (Docker parity); stderr: {}",
        String::from_utf8_lossy(&exec_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&exec_out.stderr).contains("No such container"),
        "exec nope stderr must contain 'No such container'; got: {}",
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
