use super::common::*;
use super::helpers::*;

use std::time::Duration;

use tempfile::TempDir;

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
