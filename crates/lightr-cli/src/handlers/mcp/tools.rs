use lightr_index::{hydrate, snapshot, status};
use lightr_run::{run_memoized, Mount, RunSpec};
use lightr_store::Store;
use serde_json::{json, Value};

use super::tool_result;

pub(super) fn handle_tools_call(id: Value, params: &Value) -> Value {
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
                // WP-RC-1: the MCP run tool has no `-e`/`--env-file` → no
                // explicit env (key unchanged for this surface).
                env_explicit: vec![],
                // WP-RC-WORKDIR: the MCP run tool has no `-w` → `None` (runs in
                // cwd; workdir is RUNTIME, not a memo-key input).
                workdir: None,
                // WP-RC-USER (NON-OWNED site, set None): the MCP run tool has no
                // `-u` → `None` (current user; user is RUNTIME, not keyed).
                user: None,
                // WP-RC-RESTART (NON-OWNED site, set None): the MCP run tool has no
                // `--restart` → `None` (run once; restart is RUNTIME, not keyed).
                restart: None,
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
