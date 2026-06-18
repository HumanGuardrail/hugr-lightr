//! `lightr mcp` handler — hand-rolled JSON-RPC 2.0 MCP server over stdio.
//!
//! Line-delimited JSON-RPC 2.0. No Content-Length header.
//! EOF → return 0.

use lightr_index::{hydrate, snapshot, status};
use lightr_run::{run_memoized, Mount, RunSpec};
use lightr_store::Store;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

pub fn run() -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stdin_lock = stdin.lock();
    let stdout_lock = stdout.lock();
    run_mcp_loop(stdin_lock, stdout_lock)
}

pub fn run_mcp_loop(mut reader: impl BufRead, mut writer: impl Write) -> i32 {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return 0, // EOF
            Ok(_) => {}
            Err(_) => return 1,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                // Parse error — send error response with null id
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": "Parse error"}
                });
                let _ = writeln!(writer, "{}", resp);
                continue;
            }
        };

        // Check if this is a notification (no id field, or id is null)
        let has_id = msg.get("id").is_some() && !msg["id"].is_null();
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));

        if !has_id {
            // Notification — no response required
            // (e.g., notifications/initialized)
            continue;
        }

        let resp = handle_method(id, method, &params);
        let _ = writeln!(writer, "{}", resp);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Method dispatch
// ─────────────────────────────────────────────────────────────────────────────

fn handle_method(id: Value, method: &str, params: &Value) -> Value {
    match method {
        "initialize" => handle_initialize(id),
        "tools/list" => handle_tools_list(id),
        "tools/call" => handle_tools_call(id, params),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "Method not found"}
        }),
    }
}

fn handle_initialize(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": "2025-03-26",
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": "lightr",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

fn handle_tools_list(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": [
                {
                    "name": "lightr_snapshot",
                    "description": "Snapshot a directory into the store under a ref",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "dir": {"type": "string", "description": "Directory to snapshot (default '.')"},
                            "name": {"type": "string", "description": "Ref name"}
                        },
                        "required": ["name"]
                    }
                },
                {
                    "name": "lightr_hydrate",
                    "description": "Materialize a ref into a directory",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "dest": {"type": "string", "description": "Destination directory"},
                            "name": {"type": "string", "description": "Ref name"},
                            "verify": {"type": "boolean", "description": "Re-hash objects before materializing"}
                        },
                        "required": ["dest", "name"]
                    }
                },
                {
                    "name": "lightr_status",
                    "description": "Compare a directory against a ref",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "dir": {"type": "string", "description": "Directory to check (default '.')"},
                            "name": {"type": "string", "description": "Ref name"}
                        },
                        "required": ["name"]
                    }
                },
                {
                    "name": "lightr_run",
                    "description": "Run a command, memoized",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "dir": {"type": "string"},
                            "command": {"type": "array", "items": {"type": "string"}},
                            "inputs": {"type": "array", "items": {"type": "string"}},
                            "env": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["command"]
                    }
                },
                {
                    "name": "lightr_diff",
                    "description": "Diff a ref against a previous version",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"},
                            "at": {"type": "integer", "description": "History index (default 1)"}
                        },
                        "required": ["name"]
                    }
                }
            ]
        }
    })
}

fn tool_result(id: Value, text: String, is_error: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": text}],
            "isError": is_error
        }
    })
}

fn handle_tools_call(id: Value, params: &Value) -> Value {
    let tool_name = match params.get("name").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32602, "message": "Missing tool name"}
            });
        }
    };

    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match tool_name {
        "lightr_snapshot" => {
            let dir = args.get("dir").and_then(|d| d.as_str()).unwrap_or(".");
            let name = match args.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => {
                    return tool_result(id, "missing required argument: name".to_string(), true)
                }
            };
            let store = match Store::open(Store::default_root()) {
                Ok(s) => s,
                Err(e) => return tool_result(id, format!("{e}"), true),
            };
            match snapshot(std::path::Path::new(dir), &store, name) {
                Ok(r) => {
                    let text = serde_json::to_string(&serde_json::json!({
                        "root": r.root.to_hex(),
                        "files": r.files,
                        "bytes_total": r.bytes_total,
                        "objects_new": r.objects_new,
                    }))
                    .unwrap_or_default();
                    tool_result(id, text, false)
                }
                Err(e) => tool_result(id, format!("{e}"), true),
            }
        }
        "lightr_hydrate" => {
            let dest = match args.get("dest").and_then(|d| d.as_str()) {
                Some(d) => d,
                None => {
                    return tool_result(id, "missing required argument: dest".to_string(), true)
                }
            };
            let name = match args.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => {
                    return tool_result(id, "missing required argument: name".to_string(), true)
                }
            };
            let store = match Store::open(Store::default_root()) {
                Ok(s) => s,
                Err(e) => return tool_result(id, format!("{e}"), true),
            };
            let result = if args
                .get("verify")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                lightr_index::hydrate_verified(std::path::Path::new(dest), &store, name)
            } else {
                hydrate(std::path::Path::new(dest), &store, name)
            };
            match result {
                Ok(r) => {
                    let rung_str = format!("{:?}", r.rung).to_lowercase();
                    let text = serde_json::to_string(&serde_json::json!({
                        "root": r.root.to_hex(),
                        "files": r.files,
                        "bytes_total": r.bytes_total,
                        "rung": rung_str,
                    }))
                    .unwrap_or_default();
                    tool_result(id, text, false)
                }
                Err(e) => tool_result(id, format!("{e}"), true),
            }
        }
        "lightr_status" => {
            let dir = args.get("dir").and_then(|d| d.as_str()).unwrap_or(".");
            let name = match args.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => {
                    return tool_result(id, "missing required argument: name".to_string(), true)
                }
            };
            let store = match Store::open(Store::default_root()) {
                Ok(s) => s,
                Err(e) => return tool_result(id, format!("{e}"), true),
            };
            match status(std::path::Path::new(dir), &store, name) {
                Ok(r) => {
                    let text = serde_json::to_string(&serde_json::json!({
                        "clean": r.clean,
                        "added": r.added,
                        "removed": r.removed,
                        "changed": r.changed,
                    }))
                    .unwrap_or_default();
                    tool_result(id, text, false)
                }
                Err(e) => tool_result(id, format!("{e}"), true),
            }
        }
        "lightr_run" => {
            let dir = args.get("dir").and_then(|d| d.as_str()).unwrap_or(".");
            let command: Vec<String> = args
                .get("command")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if command.is_empty() {
                return tool_result(id, "missing required argument: command".to_string(), true);
            }
            let inputs: Vec<String> = args
                .get("inputs")
                .and_then(|i| i.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let env_keys: Vec<String> = args
                .get("env")
                .and_then(|e| e.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let store = match Store::open(Store::default_root()) {
                Ok(s) => s,
                Err(e) => return tool_result(id, format!("{e}"), true),
            };
            let cwd = std::path::PathBuf::from(dir);
            let input_paths: Vec<std::path::PathBuf> = if inputs.is_empty() {
                vec![cwd.clone()]
            } else {
                inputs.iter().map(std::path::PathBuf::from).collect()
            };
            let spec = RunSpec {
                cwd,
                inputs: input_paths,
                command,
                env_keys,
                mounts: vec![Mount {
                    ref_name: String::new(),
                    target: String::new(),
                }]
                .into_iter()
                .filter(|_| false)
                .collect(),
                secrets: vec![],
                configs: vec![],
                ports: vec![],
            };
            match run_memoized(&spec, &store) {
                Ok(outcome) => {
                    let text = serde_json::to_string(&serde_json::json!({
                        "key": outcome.key.to_hex(),
                        "hit": outcome.hit,
                        "exit_code": outcome.exit_code,
                    }))
                    .unwrap_or_default();
                    tool_result(id, text, false)
                }
                Err(e) => tool_result(id, format!("{e}"), true),
            }
        }
        "lightr_diff" => {
            let name = match args.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => {
                    return tool_result(id, "missing required argument: name".to_string(), true)
                }
            };
            let at = args.get("at").and_then(|a| a.as_u64()).unwrap_or(1) as usize;

            let store = match Store::open(Store::default_root()) {
                Ok(s) => s,
                Err(e) => return tool_result(id, format!("{e}"), true),
            };
            let ref_log = match store.ref_log(name) {
                Ok(log) if !log.is_empty() => log,
                Ok(_) => return tool_result(id, format!("ref not found: {name}"), true),
                Err(e) => return tool_result(id, format!("{e}"), true),
            };

            let current_manifest = match store.get_bytes(&ref_log[0].root) {
                Ok(bytes) => match lightr_core::Manifest::decode(&bytes) {
                    Ok(m) => m,
                    Err(e) => return tool_result(id, format!("{e}"), true),
                },
                Err(e) => return tool_result(id, format!("{e}"), true),
            };

            if ref_log.len() <= at {
                return tool_result(id, format!("not enough history (need index {at})"), true);
            }

            let old_manifest = match store.get_bytes(&ref_log[at].root) {
                Ok(bytes) => match lightr_core::Manifest::decode(&bytes) {
                    Ok(m) => m,
                    Err(e) => return tool_result(id, format!("{e}"), true),
                },
                Err(e) => return tool_result(id, format!("{e}"), true),
            };

            let report = lightr_index::diff_manifests(&old_manifest, &current_manifest);
            let text = serde_json::to_string(&serde_json::json!({
                "added": report.added,
                "removed": report.removed,
                "changed": report.changed,
            }))
            .unwrap_or_default();
            tool_result(id, text, false)
        }
        _ => tool_result(id, format!("unknown tool: {tool_name}"), true),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn mcp_initialize_responds_with_protocol_version() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#.to_string() + "\n";
        let mut output = Vec::<u8>::new();
        let reader = Cursor::new(input.as_bytes().to_vec());
        run_mcp_loop(reader, &mut output);
        let resp = String::from_utf8(output).unwrap();
        assert!(
            resp.contains(r#""protocolVersion":"2025-03-26""#),
            "missing protocolVersion: {resp}"
        );
        assert!(
            resp.contains(r#""name":"lightr""#),
            "missing server name: {resp}"
        );
    }

    #[test]
    fn mcp_tools_list_returns_five_tools() {
        let input =
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#.to_string() + "\n";
        let mut output = Vec::<u8>::new();
        let reader = Cursor::new(input.as_bytes().to_vec());
        run_mcp_loop(reader, &mut output);
        let resp = String::from_utf8(output).unwrap();
        let count = [
            "lightr_snapshot",
            "lightr_hydrate",
            "lightr_status",
            "lightr_run",
            "lightr_diff",
        ]
        .iter()
        .filter(|&&n| resp.contains(n))
        .count();
        assert_eq!(count, 5, "expected 5 tools in response: {resp}");
    }

    #[test]
    fn mcp_unknown_method_returns_32601() {
        let input =
            r#"{"jsonrpc":"2.0","id":3,"method":"unknown/method","params":{}}"#.to_string() + "\n";
        let mut output = Vec::<u8>::new();
        let reader = Cursor::new(input.as_bytes().to_vec());
        run_mcp_loop(reader, &mut output);
        let resp = String::from_utf8(output).unwrap();
        assert!(resp.contains("-32601"), "expected -32601 error: {resp}");
    }

    #[test]
    fn mcp_eof_returns_0() {
        let input = b"";
        let mut output = Vec::<u8>::new();
        let reader = Cursor::new(input.to_vec());
        let code = run_mcp_loop(reader, &mut output);
        assert_eq!(code, 0);
    }

    #[test]
    fn mcp_notification_no_response() {
        let input = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#
            .to_string()
            + "\n";
        let mut output = Vec::<u8>::new();
        let reader = Cursor::new(input.as_bytes().to_vec());
        run_mcp_loop(reader, &mut output);
        // no output for notifications
        assert!(
            output.is_empty(),
            "notifications must not produce output, got: {:?}",
            String::from_utf8_lossy(&output)
        );
    }
}
