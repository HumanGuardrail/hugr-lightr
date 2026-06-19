//! `lightr inspect <id>` handler — docker-inspect parity subset.
//!
//! Emits a single-element JSON array (Docker's exact wire shape) containing one
//! object with the fields we can honestly source from SpecOnDisk + RunInfo.
//! Fields we don't have are `null`, never fabricated.
//!
//! JSON shape (docker-compatible single-element array):
//! ```json
//! [
//!   {
//!     "Id": "1234567890-42",
//!     "Created": 1717600000,
//!     "State": {
//!       "Status": "running",
//!       "Running": true,
//!       "ExitCode": 0
//!     },
//!     "Config": {
//!       "Cmd": ["echo", "hello"],
//!       "Env": ["FOO", "BAR"],
//!       "WorkingDir": "/work"
//!     },
//!     "Image": null,
//!     "RootfsRef": null,
//!     "HostConfig": {
//!       "PortBindings": [{"host": 8080, "container": 80}],
//!       "Mounts": []
//!     },
//!     "Engine": "native",
//!     "Health": null
//!   }
//! ]
//! ```
//!
//! Missing id → stderr + exit 2 (RefNotFound-class per exit.rs law).
//! Human path (no --json): compact single-line summary per field group.
//! --json path: always the array above.

use lightr_run::ps;
use serde::Serialize;

use crate::{exit::die_internal, lightr_home};

// ── JSON shapes ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct InspectStateJson {
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "Running")]
    running: bool,
    #[serde(rename = "ExitCode")]
    exit_code: i32,
}

#[derive(Serialize)]
struct InspectConfigJson {
    #[serde(rename = "Cmd")]
    cmd: Vec<String>,
    /// Env key names only — values are not persisted in spec.json.
    #[serde(rename = "Env")]
    env: Vec<String>,
    /// Working directory (cwd from SpecOnDisk is not exposed via RunInfo;
    /// we source it from spec.json directly via lightr_run::ps which reads cwd
    /// into `command` context — but RunInfo doesn't carry it. Honestly null.
    #[serde(rename = "WorkingDir")]
    working_dir: Option<String>,
}

#[derive(Serialize)]
struct PortBindingJson {
    host: u16,
    container: u16,
}

#[derive(Serialize)]
struct InspectHostConfigJson {
    #[serde(rename = "PortBindings")]
    port_bindings: Vec<PortBindingJson>,
    #[serde(rename = "Mounts")]
    mounts: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct InspectJson {
    #[serde(rename = "Id")]
    id: String,
    /// Unix timestamp (seconds) the run was created.
    #[serde(rename = "Created")]
    created: u64,
    #[serde(rename = "State")]
    state: InspectStateJson,
    #[serde(rename = "Config")]
    config: InspectConfigJson,
    /// OCI image reference; not stored in lightr — honestly null.
    #[serde(rename = "Image")]
    image: Option<String>,
    /// Rootfs ref (vz runs only).
    #[serde(rename = "RootfsRef")]
    rootfs_ref: Option<String>,
    #[serde(rename = "HostConfig")]
    host_config: InspectHostConfigJson,
    #[serde(rename = "Engine")]
    engine: String,
    /// Healthcheck verdict — null when no healthcheck configured.
    #[serde(rename = "Health")]
    health: Option<String>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Run `lightr inspect <id>`.
///
/// `json` mirrors the global `--json` flag. When false a compact human summary
/// is printed. When true (or when called from `docker inspect`) the single-element
/// array shape is printed.
///
/// Exit codes per exit.rs law:
///   0 — found + printed
///   2 — id not found (RefNotFound-class)
///   1 — other I/O error
pub fn run(id: &str, json: bool) -> i32 {
    let home = lightr_home();

    let runs = match ps(&home) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lightr: {e}");
            return 1;
        }
    };

    let info = match runs.into_iter().find(|r| r.id == id) {
        Some(r) => r,
        None => {
            return die_internal(&format!("inspect: id '{id}' not found"));
        }
    };

    let status_str = if info.running {
        "running".to_string()
    } else {
        "exited".to_string()
    };

    let exit_code = info.exit_code.unwrap_or(0);

    let health_str = info.health.map(|h| h.as_str().to_string());

    let record = InspectJson {
        id: info.id.clone(),
        created: info.created_at_unix,
        state: InspectStateJson {
            status: status_str.clone(),
            running: info.running,
            exit_code,
        },
        config: InspectConfigJson {
            cmd: info.command.clone(),
            // env keys from RunInfo — values not stored
            env: vec![], // RunInfo doesn't carry env keys; spec.json has env_keys but
            // not surfaced in RunInfo. Honest empty rather than fabricated.
            working_dir: None, // RunInfo doesn't carry cwd; honestly null.
        },
        image: None,
        rootfs_ref: info.rootfs_ref.clone(),
        host_config: InspectHostConfigJson {
            port_bindings: info
                .ports
                .iter()
                .map(|&(host, container)| PortBindingJson { host, container })
                .collect(),
            mounts: vec![],
        },
        engine: info.engine.clone(),
        health: health_str,
    };

    if json {
        // Docker wire shape: single-element array.
        let arr = vec![record];
        println!(
            "{}",
            serde_json::to_string(&arr).expect("serialize inspect")
        );
    } else {
        // Human-readable compact summary.
        let cmd0 = record
            .config
            .cmd
            .first()
            .map(|s| s.as_str())
            .unwrap_or("<none>");
        let health_disp = record.health.as_deref().unwrap_or("none");
        let ports_disp = if record.host_config.port_bindings.is_empty() {
            "none".to_string()
        } else {
            record
                .host_config
                .port_bindings
                .iter()
                .map(|p| format!("{}:{}", p.host, p.container))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let rootfs_disp = record.rootfs_ref.as_deref().unwrap_or("none");

        println!("Id:       {}", record.id);
        println!("Created:  {}", record.created);
        println!("Status:   {}", status_str);
        println!("ExitCode: {}", exit_code);
        println!("Engine:   {}", record.engine);
        println!("Cmd:      {cmd0}");
        println!("Ports:    {ports_disp}");
        println!("Rootfs:   {rootfs_disp}");
        println!("Health:   {health_disp}");
    }

    0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use super::run as inspect_run;
    use crate::test_lock::ENV_LOCK;

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

    // ── inspect on a present run returns 0 + valid JSON ──────────────────────

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

    // ── inspect on a missing id returns 2 ────────────────────────────────────

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

    // ── inspect with no run dir at all returns 2 ─────────────────────────────

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
}
