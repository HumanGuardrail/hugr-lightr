//! Tests for the `lightr inspect` handler — split out via `#[path]` to keep
//! inspect.rs under the 400-line godfile cap (house convention).
//!
//! Two groups:
//!   1. Existing WP-INS1 exit-code contract (preserved verbatim): present run ⇒ 0,
//!      missing id ⇒ 2, no run dir ⇒ 2, human path ⇒ 0.
//!   2. WP-INSPECT-ENRICH: the now-populated run-config fields are surfaced in the
//!      Docker-faithful inspect locations, None/empty fields omitted/defaulted.
//!
//! Every test is parallel-safe: each uses its own tempdir; the end-to-end
//! `inspect_run` calls hold the crate-wide `ENV_LOCK` while LIGHTR_HOME is set
//! (process-global). The enrichment assertions exercise the pure mapping helpers
//! (`read_spec`/`restart_policy`) and serialize the private JSON structs directly,
//! so they touch no process-global state at all.

use std::fs;

use super::run as inspect_run;
use super::{read_spec, restart_policy, InspectConfigJson, InspectHostConfigJson, PortBindingJson};
use crate::test_lock::ENV_LOCK;
use std::collections::BTreeMap;

/// Create a minimal run directory with spec.json and optionally a status file.
fn make_run_dir(base: &std::path::Path, id: &str, exit_code: Option<i32>) {
    let run_dir = base.join("run").join(id);
    fs::create_dir_all(&run_dir).unwrap();

    let spec = serde_json::json!({
        "cwd": "/work",
        "command": ["echo", "hello"],
        "env_keys": [],
        "mounts": [],
        "detached": false,
        "created_at_unix": 1_717_600_000u64,
        "ports": [],
        "engine": "native",
        "rootfs_ref": null,
        "env": []
    });
    fs::write(run_dir.join("spec.json"), spec.to_string()).unwrap();

    if let Some(code) = exit_code {
        fs::write(run_dir.join("status"), format!("exit:{code}")).unwrap();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 1 — existing WP-INS1 exit-code contract (UNCHANGED)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn inspect_found_json_contains_id_and_state() {
    let tmp = tempfile::tempdir().unwrap();
    let id = "1717600000000000000-42";
    make_run_dir(tmp.path(), id, Some(0));

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: single-threaded under ENV_LOCK
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };

    // Capture stdout by redirecting through the function and reading back from
    // a temp file is complex; instead we call with json=true and verify exit 0.
    let code = inspect_run(id, true);

    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 0, "inspect on known id must return 0");
}

#[test]
fn inspect_found_human_returns_0() {
    let tmp = tempfile::tempdir().unwrap();
    let id = "1717600000000000001-99";
    make_run_dir(tmp.path(), id, Some(0));

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };

    let code = inspect_run(id, false);

    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 0, "inspect (human) on known id must return 0");
}

#[test]
fn inspect_missing_id_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    // Don't create any run dirs — just an empty LIGHTR_HOME.
    fs::create_dir_all(tmp.path().join("run")).unwrap();

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };

    let code = inspect_run("no-such-id", true);

    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 2, "inspect on unknown id must return 2");
}

#[test]
fn inspect_no_run_dir_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    // LIGHTR_HOME exists but no `run/` subdir — ps() returns empty Vec.

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };

    let code = inspect_run("any-id", true);

    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 2, "inspect with no run dir must return 2");
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 2 — WP-INSPECT-ENRICH: enriched run-config fields
// ─────────────────────────────────────────────────────────────────────────────

/// Build the `Config` object the handler emits for a given spec — mirrors the
/// mapping in `run()` so the test asserts the exact serialized shape.
fn config_for(spec: &super::InspectSpec) -> InspectConfigJson {
    let labels = if spec.labels.is_empty() {
        None
    } else {
        Some(spec.labels.iter().cloned().collect::<BTreeMap<_, _>>())
    };
    let env: Vec<String> = spec
        .env_explicit
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    InspectConfigJson {
        cmd: vec!["echo".into(), "hello".into()],
        env,
        working_dir: spec.workdir.clone(),
        user: spec.user.clone(),
        hostname: spec.hostname.clone(),
        stop_signal: spec.stop_signal.clone(),
        labels,
    }
}

/// A run with workdir/user/restart/stop_signal/labels/hostname/env all set ⇒
/// every field lands in its Docker-faithful location with Docker key names.
#[test]
fn enriched_spec_surfaces_all_config_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run").join("id-full");
    fs::create_dir_all(&run_dir).unwrap();
    let spec_json = serde_json::json!({
        "cwd": "/work",
        "command": ["echo", "hello"],
        "created_at_unix": 1_717_600_000u64,
        "workdir": "/app",
        "user": "1000:1000",
        "hostname": "web",
        "stop_signal": "SIGTERM",
        "restart": "on-failure:3",
        "labels": [["role", "web"], ["tier", "frontend"]],
        "env_explicit": [["FOO", "bar"], ["BAZ", "qux"]]
    });
    fs::write(run_dir.join("spec.json"), spec_json.to_string()).unwrap();

    let spec = read_spec(&run_dir);
    assert_eq!(spec.workdir.as_deref(), Some("/app"));
    assert_eq!(spec.user.as_deref(), Some("1000:1000"));
    assert_eq!(spec.hostname.as_deref(), Some("web"));
    assert_eq!(spec.stop_signal.as_deref(), Some("SIGTERM"));
    assert_eq!(spec.restart.as_deref(), Some("on-failure:3"));

    // Config object — Docker-faithful key names + nesting.
    let cfg = serde_json::to_value(config_for(&spec)).unwrap();
    assert_eq!(cfg["WorkingDir"], "/app");
    assert_eq!(cfg["User"], "1000:1000");
    assert_eq!(cfg["Hostname"], "web");
    assert_eq!(cfg["StopSignal"], "SIGTERM");
    assert_eq!(cfg["Labels"]["role"], "web");
    assert_eq!(cfg["Labels"]["tier"], "frontend");
    // Env: Docker `["KEY=value", …]`.
    assert_eq!(cfg["Env"], serde_json::json!(["FOO=bar", "BAZ=qux"]));

    // HostConfig.RestartPolicy — {Name, MaximumRetryCount}.
    let hc = InspectHostConfigJson {
        port_bindings: vec![PortBindingJson {
            host: 8080,
            container: 80,
        }],
        restart_policy: restart_policy(spec.restart.as_deref()),
        mounts: vec![],
    };
    let hc = serde_json::to_value(hc).unwrap();
    assert_eq!(hc["RestartPolicy"]["Name"], "on-failure");
    assert_eq!(hc["RestartPolicy"]["MaximumRetryCount"], 3);
    assert_eq!(hc["PortBindings"][0]["host"], 8080);
}

/// None/empty fields are OMITTED (never fabricated) — and the back-compat
/// spec.json (no enriched fields at all) parses to all-defaults.
#[test]
fn unset_fields_are_omitted_or_defaulted() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run").join("id-bare");
    fs::create_dir_all(&run_dir).unwrap();
    // A pre-freeze spec.json with none of the enriched fields.
    let spec_json = serde_json::json!({
        "cwd": "/work",
        "command": ["echo", "hello"],
        "created_at_unix": 1_717_600_000u64
    });
    fs::write(run_dir.join("spec.json"), spec_json.to_string()).unwrap();

    let spec = read_spec(&run_dir);
    assert!(spec.workdir.is_none());
    assert!(spec.labels.is_empty());
    assert!(spec.env_explicit.is_empty());

    let cfg = serde_json::to_value(config_for(&spec)).unwrap();
    let obj = cfg.as_object().unwrap();
    // Omitted, not null.
    assert!(
        !obj.contains_key("WorkingDir"),
        "WorkingDir must be omitted"
    );
    assert!(!obj.contains_key("User"), "User must be omitted");
    assert!(!obj.contains_key("Hostname"), "Hostname must be omitted");
    assert!(
        !obj.contains_key("StopSignal"),
        "StopSignal must be omitted"
    );
    assert!(!obj.contains_key("Labels"), "Labels must be omitted");
    // Env stays present as an empty array (Docker always emits Config.Env).
    assert_eq!(cfg["Env"], serde_json::json!([]));
    // Cmd preserved.
    assert_eq!(cfg["Cmd"], serde_json::json!(["echo", "hello"]));
}

/// A missing / unreadable spec.json ⇒ all-default spec (fail-closed), never a panic.
#[test]
fn missing_spec_json_yields_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run").join("id-nospec");
    fs::create_dir_all(&run_dir).unwrap();
    // No spec.json written.
    let spec = read_spec(&run_dir);
    assert!(spec.workdir.is_none());
    assert!(spec.restart.is_none());
    assert!(spec.labels.is_empty());
}

/// `restart_policy` maps every Docker `--restart` form to {Name, MaximumRetryCount}.
#[test]
fn restart_policy_maps_docker_forms() {
    // None ⇒ Docker default {"no", 0} — ALWAYS emitted.
    let p = restart_policy(None);
    assert_eq!(p.name, "no");
    assert_eq!(p.maximum_retry_count, 0);

    let p = restart_policy(Some("always"));
    assert_eq!(p.name, "always");
    assert_eq!(p.maximum_retry_count, 0);

    let p = restart_policy(Some("unless-stopped"));
    assert_eq!(p.name, "unless-stopped");
    assert_eq!(p.maximum_retry_count, 0);

    let p = restart_policy(Some("on-failure:5"));
    assert_eq!(p.name, "on-failure");
    assert_eq!(p.maximum_retry_count, 5);

    // Bare on-failure (no count) ⇒ 0.
    let p = restart_policy(Some("on-failure"));
    assert_eq!(p.name, "on-failure");
    assert_eq!(p.maximum_retry_count, 0);

    // Empty string ⇒ "no" (defensive).
    let p = restart_policy(Some(""));
    assert_eq!(p.name, "no");
    assert_eq!(p.maximum_retry_count, 0);
}

/// End-to-end: an enriched run resolves through `inspect_run` (json path) ⇒ 0,
/// proving the full handler reads spec.json from the run dir without error.
#[test]
fn inspect_run_enriched_returns_0() {
    let tmp = tempfile::tempdir().unwrap();
    let id = "1717600000000000002-7";
    let run_dir = tmp.path().join("run").join(id);
    fs::create_dir_all(&run_dir).unwrap();
    // The non-defaulted SpecOnDisk fields (cwd/command/env_keys/mounts/detached/
    // created_at_unix) must be present or `ps` skips the run; the enriched fields
    // ride alongside.
    let spec_json = serde_json::json!({
        "cwd": "/work",
        "command": ["echo", "hello"],
        "env_keys": [],
        "mounts": [],
        "detached": false,
        "created_at_unix": 1_717_600_000u64,
        "workdir": "/app",
        "user": "root",
        "hostname": "svc",
        "stop_signal": "SIGINT",
        "restart": "always",
        "labels": [["k", "v"]],
        "env_explicit": [["A", "1"]]
    });
    fs::write(run_dir.join("spec.json"), spec_json.to_string()).unwrap();
    fs::write(run_dir.join("status"), "exit:0").unwrap();

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = inspect_run(id, true);
    unsafe { std::env::remove_var("LIGHTR_HOME") };

    assert_eq!(code, 0, "inspect on an enriched run must return 0");
}
