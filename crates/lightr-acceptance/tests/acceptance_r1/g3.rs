use super::common::*;

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use tempfile::TempDir;

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
