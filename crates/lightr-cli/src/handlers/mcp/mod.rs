//! `lightr mcp` handler — hand-rolled JSON-RPC 2.0 MCP server over stdio.
//!
//! Line-delimited JSON-RPC 2.0. No Content-Length header.
//! EOF → return 0.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

mod tools;

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
        "tools/call" => tools::handle_tools_call(id, params),
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

pub(crate) fn tool_result(id: Value, text: String, is_error: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": text}],
            "isError": is_error
        }
    })
}

#[cfg(test)]
mod tests;
