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

fn schema_inspect() -> Value {
    // Single-element array containing one InspectJson object (docker wire shape).
    // Fields sourced from SpecOnDisk + RunInfo; Image/WorkingDir/Env are honestly null/[].
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-lightr-schema-version": 1,
        "type": "array",
        "minItems": 1,
        "maxItems": 1,
        "items": {
            "type": "object",
            "properties": {
                "Id":      { "type": "string", "description": "run id (unix_nanos-pid)" },
                "Created": { "type": "integer", "minimum": 0, "description": "created_at_unix (seconds)" },
                "State": {
                    "type": "object",
                    "properties": {
                        "Status":   { "type": "string", "enum": ["running", "exited"] },
                        "Running":  { "type": "boolean" },
                        "ExitCode": { "type": "integer" }
                    },
                    "required": ["Status", "Running", "ExitCode"]
                },
                "Config": {
                    "type": "object",
                    "properties": {
                        "Cmd":        { "type": "array", "items": { "type": "string" } },
                        "Env":        { "type": "array", "items": { "type": "string" },
                                        "description": "env key names only; values not persisted" },
                        "WorkingDir": { "type": ["string", "null"],
                                        "description": "null — not surfaced by RunInfo" }
                    },
                    "required": ["Cmd", "Env", "WorkingDir"]
                },
                "Image":     { "type": ["string", "null"],
                               "description": "null — lightr does not store OCI image refs" },
                "RootfsRef": { "type": ["string", "null"],
                               "description": "vz rootfs ref; null for native runs" },
                "HostConfig": {
                    "type": "object",
                    "properties": {
                        "PortBindings": {
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
                        "Mounts": { "type": "array" }
                    },
                    "required": ["PortBindings", "Mounts"]
                },
                "Engine": { "type": "string", "description": "engine kind: native or vz" },
                "Health":  { "type": ["string", "null"],
                             "enum": ["healthy", "unhealthy", null],
                             "description": "null when no healthcheck configured" }
            },
            "required": ["Id", "Created", "State", "Config", "Image", "RootfsRef",
                         "HostConfig", "Engine", "Health"]
        }
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

pub(crate) const KNOWN_VERBS: &[&str] = &[
    "snapshot", "hydrate", "status", "run", "diff", "gc", "ps", "inspect",
];

pub(crate) fn schema_for(verb: &str) -> Option<Value> {
    match verb {
        "snapshot" => Some(schema_snapshot()),
        "hydrate" => Some(schema_hydrate()),
        "status" => Some(schema_status()),
        "run" => Some(schema_run()),
        "diff" => Some(schema_diff()),
        "gc" => Some(schema_gc()),
        "ps" => Some(schema_ps()),
        "inspect" => Some(schema_inspect()),
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

#[cfg(test)]
mod tests;
