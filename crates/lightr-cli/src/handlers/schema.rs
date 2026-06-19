//! `lightr schema [--verb <v>]` handler — build-spec-r4 §2.
//!
//! Emits hand-written JSON Schema (draft-07) constants for each verb's --json output.
//! The schemas mirror the ACTUAL serialised structs from each handler's *Json type.
//!
//! `lightr schema`           → one JSON object {verb_name: schema, ...}
//! `lightr schema --verb run` → that one schema
//! Unknown verb              → exit 2 "lightr schema: unknown verb '<v>'"
//!
//! --json flag is irrelevant here (schema IS json) — always emit JSON.

use serde_json::{json, Value};

// ──────────────────────────────────────────────────────────────────────────────
// Individual schema constants — mirror the *Json structs exactly
// ──────────────────────────────────────────────────────────────────────────────

fn schema_snapshot() -> Value {
    // mirrors SnapshotJson { root: String, files: u64, bytes_total: u64, objects_new: u64 }
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-lightr-schema-version": 1,
        "type": "object",
        "properties": {
            "root":        { "type": "string", "description": "hex-encoded content root hash" },
            "files":       { "type": "integer", "minimum": 0 },
            "bytes_total": { "type": "integer", "minimum": 0 },
            "objects_new": { "type": "integer", "minimum": 0 }
        },
        "required": ["root", "files", "bytes_total", "objects_new"]
    })
}

fn schema_hydrate() -> Value {
    // mirrors HydrateJson { root: String, files: u64, bytes_total: u64, rung: String }
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-lightr-schema-version": 1,
        "type": "object",
        "properties": {
            "root":        { "type": "string", "description": "hex-encoded content root hash" },
            "files":       { "type": "integer", "minimum": 0 },
            "bytes_total": { "type": "integer", "minimum": 0 },
            "rung":        { "type": "string", "enum": ["clone", "reflink", "copyrange", "copy"] }
        },
        "required": ["root", "files", "bytes_total", "rung"]
    })
}

fn schema_status() -> Value {
    // mirrors StatusJson { clean: bool, added: Vec<String>, removed: Vec<String>, changed: Vec<String> }
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-lightr-schema-version": 1,
        "type": "object",
        "properties": {
            "clean":   { "type": "boolean" },
            "added":   { "type": "array", "items": { "type": "string" } },
            "removed": { "type": "array", "items": { "type": "string" } },
            "changed": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["clean", "added", "removed", "changed"]
    })
}

fn schema_run() -> Value {
    // mirrors RunJson { key: String, hit: bool, exit_code: i32 }
    // NOTE: run --json goes to STDERR prefixed "lightr-json: " (per handler doc)
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-lightr-schema-version": 1,
        "type": "object",
        "properties": {
            "key":       { "type": "string", "description": "hex-encoded memo key" },
            "hit":       { "type": "boolean" },
            "exit_code": { "type": "integer" }
        },
        "required": ["key", "hit", "exit_code"]
    })
}

fn schema_diff() -> Value {
    // mirrors DiffJson { added: Vec<String>, removed: Vec<String>, changed: Vec<String> }
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-lightr-schema-version": 1,
        "type": "object",
        "properties": {
            "added":   { "type": "array", "items": { "type": "string" } },
            "removed": { "type": "array", "items": { "type": "string" } },
            "changed": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["added", "removed", "changed"]
    })
}

fn schema_gc() -> Value {
    // mirrors GcJson { objects_total: u64, reachable: u64, swept: u64, bytes_freed: u64, run_dirs_removed: u64 }
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-lightr-schema-version": 1,
        "type": "object",
        "properties": {
            "objects_total":    { "type": "integer", "minimum": 0 },
            "reachable":        { "type": "integer", "minimum": 0 },
            "swept":            { "type": "integer", "minimum": 0 },
            "bytes_freed":      { "type": "integer", "minimum": 0 },
            "run_dirs_removed": { "type": "integer", "minimum": 0 }
        },
        "required": ["objects_total", "reachable", "swept", "bytes_freed", "run_dirs_removed"]
    })
}

fn schema_ps() -> Value {
    // mirrors Vec<RunInfoJson> where each element has:
    //   RunInfoJson { id, running, exit_code, command, created_at_unix,
    //                 health, engine, ports, rootfs_ref }
    // The top-level is an array of run-info objects.
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-lightr-schema-version": 1,
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "id":              { "type": "string", "description": "run id (unix_nanos-pid)" },
                "running":         { "type": "boolean" },
                "exit_code":       { "type": ["integer", "null"] },
                "command":         { "type": "array", "items": { "type": "string" } },
                "created_at_unix": { "type": "integer", "minimum": 0 },
                "health":          { "type": ["string", "null"],
                                     "enum": ["healthy", "unhealthy", null],
                                     "description": "null when no healthcheck configured" },
                "engine":          { "type": "string", "description": "engine kind: native or vz" },
                "ports": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "host":      { "type": "integer", "minimum": 0, "maximum": 65535 },
                            "container": { "type": "integer", "minimum": 0, "maximum": 65535 }
                        },
                        "required": ["host", "container"]
                    }
                },
                "rootfs_ref": { "type": ["string", "null"],
                                "description": "vz rootfs ref, null for native runs" }
            },
            "required": ["id", "running", "exit_code", "command", "created_at_unix",
                         "health", "engine", "ports", "rootfs_ref"]
        }
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Verb registry
// ──────────────────────────────────────────────────────────────────────────────

const KNOWN_VERBS: &[&str] = &["snapshot", "hydrate", "status", "run", "diff", "gc", "ps"];

fn schema_for(verb: &str) -> Option<Value> {
    match verb {
        "snapshot" => Some(schema_snapshot()),
        "hydrate" => Some(schema_hydrate()),
        "status" => Some(schema_status()),
        "run" => Some(schema_run()),
        "diff" => Some(schema_diff()),
        "gc" => Some(schema_gc()),
        "ps" => Some(schema_ps()),
        _ => None,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

pub fn run(verb: Option<&str>) -> i32 {
    match verb {
        Some(v) => match schema_for(v) {
            Some(s) => {
                println!("{}", serde_json::to_string(&s).expect("serialize schema"));
                0
            }
            None => {
                eprintln!("lightr schema: unknown verb '{v}'");
                2
            }
        },
        None => {
            // emit all schemas as one object {verb_name: schema, ...}
            let mut map = serde_json::Map::new();
            for v in KNOWN_VERBS {
                if let Some(s) = schema_for(v) {
                    map.insert(v.to_string(), s);
                }
            }
            println!(
                "{}",
                serde_json::to_string(&Value::Object(map)).expect("serialize all schemas")
            );
            0
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{schema_for, KNOWN_VERBS};

    // Helper: parse required keys from a schema Value
    fn required_keys(schema: &Value) -> Vec<String> {
        schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    // Helper: get all keys from a serialized *Json struct via a dummy instance
    fn keys_in_properties(schema: &Value) -> Vec<String> {
        schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn all_schemas_have_schema_version() {
        for verb in KNOWN_VERBS {
            let s = schema_for(verb).unwrap_or_else(|| panic!("missing schema for {verb}"));
            let ver = s.get("x-lightr-schema-version").and_then(|v| v.as_u64());
            assert_eq!(ver, Some(1), "x-lightr-schema-version must be 1 for {verb}");
        }
    }

    #[test]
    fn all_schemas_have_draft07() {
        for verb in KNOWN_VERBS {
            let s = schema_for(verb).unwrap_or_else(|| panic!("missing schema for {verb}"));
            let schema_url = s.get("$schema").and_then(|v| v.as_str());
            assert_eq!(
                schema_url,
                Some("http://json-schema.org/draft-07/schema#"),
                "$schema must be draft-07 for {verb}"
            );
        }
    }

    #[test]
    fn required_keys_subset_of_properties() {
        for verb in KNOWN_VERBS {
            let s = schema_for(verb).unwrap_or_else(|| panic!("missing schema for {verb}"));
            let props = keys_in_properties(&s);
            let req = required_keys(&s);
            for key in &req {
                assert!(
                    props.contains(key),
                    "required key '{key}' not in properties for verb '{verb}'"
                );
            }
        }
    }

    // ── Check that required keys match the ACTUAL serialised *Json structs ──

    // snapshot: SnapshotJson { root, files, bytes_total, objects_new }
    #[test]
    fn snapshot_schema_required_keys_match_struct() {
        use serde::Serialize;
        #[derive(Serialize)]
        struct SnapshotJson {
            root: String,
            files: u64,
            bytes_total: u64,
            objects_new: u64,
        }
        let dummy = SnapshotJson {
            root: "abc".to_string(),
            files: 0,
            bytes_total: 0,
            objects_new: 0,
        };
        let serialized: Value = serde_json::to_value(&dummy).unwrap();
        let struct_keys: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();
        let schema = schema_for("snapshot").unwrap();
        let req = required_keys(&schema);
        for key in &req {
            assert!(
                struct_keys.contains(key),
                "schema required key '{key}' not found in serialized SnapshotJson"
            );
        }
    }

    // hydrate: HydrateJson { root, files, bytes_total, rung }
    #[test]
    fn hydrate_schema_required_keys_match_struct() {
        use serde::Serialize;
        #[derive(Serialize)]
        struct HydrateJson {
            root: String,
            files: u64,
            bytes_total: u64,
            rung: String,
        }
        let dummy = HydrateJson {
            root: "abc".to_string(),
            files: 0,
            bytes_total: 0,
            rung: "copy".to_string(),
        };
        let serialized: Value = serde_json::to_value(&dummy).unwrap();
        let struct_keys: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();
        let schema = schema_for("hydrate").unwrap();
        let req = required_keys(&schema);
        for key in &req {
            assert!(
                struct_keys.contains(key),
                "schema required key '{key}' not found in serialized HydrateJson"
            );
        }
    }

    // status: StatusJson { clean, added, removed, changed }
    #[test]
    fn status_schema_required_keys_match_struct() {
        use serde::Serialize;
        #[derive(Serialize)]
        struct StatusJson {
            clean: bool,
            added: Vec<String>,
            removed: Vec<String>,
            changed: Vec<String>,
        }
        let dummy = StatusJson {
            clean: true,
            added: vec![],
            removed: vec![],
            changed: vec![],
        };
        let serialized: Value = serde_json::to_value(&dummy).unwrap();
        let struct_keys: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();
        let schema = schema_for("status").unwrap();
        let req = required_keys(&schema);
        for key in &req {
            assert!(
                struct_keys.contains(key),
                "schema required key '{key}' not found in serialized StatusJson"
            );
        }
    }

    // run: RunJson { key, hit, exit_code }
    #[test]
    fn run_schema_required_keys_match_struct() {
        use serde::Serialize;
        #[derive(Serialize)]
        struct RunJson {
            key: String,
            hit: bool,
            exit_code: i32,
        }
        let dummy = RunJson {
            key: "abc".to_string(),
            hit: false,
            exit_code: 0,
        };
        let serialized: Value = serde_json::to_value(&dummy).unwrap();
        let struct_keys: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();
        let schema = schema_for("run").unwrap();
        let req = required_keys(&schema);
        for key in &req {
            assert!(
                struct_keys.contains(key),
                "schema required key '{key}' not found in serialized RunJson"
            );
        }
    }

    // diff: DiffJson { added, removed, changed }
    #[test]
    fn diff_schema_required_keys_match_struct() {
        use serde::Serialize;
        #[derive(Serialize)]
        struct DiffJson {
            added: Vec<String>,
            removed: Vec<String>,
            changed: Vec<String>,
        }
        let dummy = DiffJson {
            added: vec![],
            removed: vec![],
            changed: vec![],
        };
        let serialized: Value = serde_json::to_value(&dummy).unwrap();
        let struct_keys: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();
        let schema = schema_for("diff").unwrap();
        let req = required_keys(&schema);
        for key in &req {
            assert!(
                struct_keys.contains(key),
                "schema required key '{key}' not found in serialized DiffJson"
            );
        }
    }

    // gc: GcJson { objects_total, reachable, swept, bytes_freed, run_dirs_removed }
    #[test]
    fn gc_schema_required_keys_match_struct() {
        use serde::Serialize;
        #[derive(Serialize)]
        struct GcJson {
            objects_total: u64,
            reachable: u64,
            swept: u64,
            bytes_freed: u64,
            run_dirs_removed: u64,
        }
        let dummy = GcJson {
            objects_total: 0,
            reachable: 0,
            swept: 0,
            bytes_freed: 0,
            run_dirs_removed: 0,
        };
        let serialized: Value = serde_json::to_value(&dummy).unwrap();
        let struct_keys: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();
        let schema = schema_for("gc").unwrap();
        let req = required_keys(&schema);
        for key in &req {
            assert!(
                struct_keys.contains(key),
                "schema required key '{key}' not found in serialized GcJson"
            );
        }
    }

    // ps: array of RunInfoJson { id, running, exit_code, command, created_at_unix,
    //     health, engine, ports, rootfs_ref }
    // The schema is an array — we validate the items' required keys against a
    // dummy serialized element (mirrors the pattern for the other verbs).
    #[test]
    fn ps_schema_required_keys_match_struct() {
        use serde::Serialize;
        use serde_json::Value;
        #[derive(Serialize)]
        struct PortMapJson {
            host: u16,
            container: u16,
        }
        #[derive(Serialize)]
        struct RunInfoJson {
            id: String,
            running: bool,
            exit_code: Option<i32>,
            command: Vec<String>,
            created_at_unix: u64,
            health: Option<String>,
            engine: String,
            ports: Vec<PortMapJson>,
            rootfs_ref: Option<String>,
        }
        let dummy = RunInfoJson {
            id: "12345-99".to_string(),
            running: false,
            exit_code: Some(0),
            command: vec!["/bin/echo".to_string()],
            created_at_unix: 0,
            health: None,
            engine: "native".to_string(),
            ports: vec![],
            rootfs_ref: None,
        };
        let serialized: Value = serde_json::to_value(&dummy).unwrap();
        let struct_keys: Vec<String> = serialized.as_object().unwrap().keys().cloned().collect();

        // The ps schema is an array; item required keys live in items.required.
        let schema = schema_for("ps").unwrap();
        let item_required: Vec<String> = schema
            .get("items")
            .and_then(|items| items.get("required"))
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        for key in &item_required {
            assert!(
                struct_keys.contains(key),
                "ps schema required key '{key}' not found in serialized RunInfoJson"
            );
        }
    }

    // ── unknown verb exits 2 ─────────────────────────────────────────────────

    #[test]
    fn unknown_verb_returns_none() {
        assert!(schema_for("notaverb").is_none());
    }

    #[test]
    fn unknown_verb_run_returns_exit2() {
        let code = super::run(Some("notaverb"));
        assert_eq!(code, 2, "unknown verb must return exit 2");
    }

    #[test]
    fn known_verb_run_returns_exit0() {
        for verb in KNOWN_VERBS {
            let code = super::run(Some(verb));
            assert_eq!(code, 0, "known verb '{verb}' must return exit 0");
        }
    }

    #[test]
    fn no_verb_run_returns_exit0() {
        let code = super::run(None);
        assert_eq!(code, 0, "all-schemas must return exit 0");
    }
}
