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
//!       "Env": ["FOO=bar"],
//!       "WorkingDir": "/work",
//!       "User": "1000:1000",
//!       "Hostname": "web",
//!       "StopSignal": "SIGTERM",
//!       "Labels": {"role": "web"}
//!     },
//!     "Image": null,
//!     "RootfsRef": null,
//!     "HostConfig": {
//!       "PortBindings": [{"host": 8080, "container": 80}],
//!       "RestartPolicy": {"Name": "on-failure", "MaximumRetryCount": 3},
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

use std::collections::BTreeMap;

use lightr_run::ps;
use serde::{Deserialize, Serialize};

use crate::{exit::die_internal, lightr_home};

// ── spec.json mirror (read-only, CLI-side) ──────────────────────────────────────
//
// `SpecOnDisk` lives in `lightr-run` and is `pub(super)` — not reachable from
// this crate. We deserialize ONLY the run-config fields inspect surfaces, each
// `#[serde(default)]` so a spec.json written before the field existed still
// parses (back-compat, mirroring SpecOnDisk's own serde defaults). The house
// convention for CLI-side spec reads is `handlers::exec::read_engine`.
#[derive(Deserialize, Default)]
struct InspectSpec {
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    stop_signal: Option<String>,
    #[serde(default)]
    restart: Option<String>,
    #[serde(default)]
    labels: Vec<(String, String)>,
    /// User `-e`/`--env-file` env — RESOLVED `(KEY, VALUE)` pairs.
    #[serde(default)]
    env_explicit: Vec<(String, String)>,
}

/// Read the inspect-relevant config fields from `spec.json` in `run_dir`.
/// Absent/unreadable/malformed ⇒ all-defaults (None/empty) — inspect then
/// omits every field, never fabricating one (fail-closed, honest null).
fn read_spec(run_dir: &std::path::Path) -> InspectSpec {
    std::fs::read(run_dir.join("spec.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Map a Docker `--restart` spec string to `(Name, MaximumRetryCount)`.
/// `None` / unrecognized ⇒ Docker's default `("no", 0)`. Only `on-failure:N`
/// carries a retry count; all others report 0 (Docker's wire behaviour).
fn restart_policy(spec: Option<&str>) -> RestartPolicyJson {
    let raw = spec.unwrap_or("no").trim();
    let (name, count) = match raw.split_once(':') {
        Some((head, tail)) if head == "on-failure" => {
            (head.to_string(), tail.trim().parse::<u32>().unwrap_or(0))
        }
        _ if raw.is_empty() => ("no".to_string(), 0),
        _ => (raw.to_string(), 0),
    };
    RestartPolicyJson {
        name,
        maximum_retry_count: count,
    }
}

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
    /// Docker `Config.Env`: `["KEY=value", …]`. Sourced from spec.json's
    /// `env_explicit` (user `-e`/`--env-file`, resolved values). Empty ⇒ `[]`.
    #[serde(rename = "Env")]
    env: Vec<String>,
    /// Docker `Config.WorkingDir` — the run's `-w`/`--workdir`. Omitted when the
    /// run set no workdir (None), never fabricated.
    #[serde(rename = "WorkingDir", skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
    /// Docker `Config.User` — the run's `-u`/`--user`. Omitted when None.
    #[serde(rename = "User", skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    /// Docker `Config.Hostname` — the run's `--hostname`. Omitted when None.
    #[serde(rename = "Hostname", skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    /// Docker `Config.StopSignal` — the run's `--stop-signal`. Omitted when None.
    #[serde(rename = "StopSignal", skip_serializing_if = "Option::is_none")]
    stop_signal: Option<String>,
    /// Docker `Config.Labels` — a string→string map. Omitted when the run set
    /// no labels (Docker emits `null`; we omit, equivalently absent).
    #[serde(rename = "Labels", skip_serializing_if = "Option::is_none")]
    labels: Option<BTreeMap<String, String>>,
}

#[derive(Serialize)]
struct PortBindingJson {
    host: u16,
    container: u16,
}

/// Docker `HostConfig.RestartPolicy`: `{ "Name": "...", "MaximumRetryCount": N }`.
/// Docker ALWAYS emits this object (default `{"no", 0}`), so unlike the omit-on-
/// None config fields we emit it unconditionally, defaulting to `no`/0.
#[derive(Serialize)]
struct RestartPolicyJson {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "MaximumRetryCount")]
    maximum_retry_count: u32,
}

#[derive(Serialize)]
struct InspectHostConfigJson {
    #[serde(rename = "PortBindings")]
    port_bindings: Vec<PortBindingJson>,
    #[serde(rename = "RestartPolicy")]
    restart_policy: RestartPolicyJson,
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

    // WP-INSPECT-ENRICH: surface the now-populated run-config fields. `ps` (via
    // RunInfo) doesn't carry them, so we read spec.json directly from the run
    // dir — the same CLI-side pattern as `handlers::exec::read_engine`.
    let run_dir = home.join("run").join(&info.id);
    let spec = read_spec(&run_dir);

    // Docker `Config.Labels` is a map; omit (≡ Docker `null`) when none.
    let labels = if spec.labels.is_empty() {
        None
    } else {
        Some(spec.labels.iter().cloned().collect::<BTreeMap<_, _>>())
    };
    // Docker `Config.Env`: `KEY=value` strings from explicit `-e`/`--env-file`.
    let env: Vec<String> = spec
        .env_explicit
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();

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
            env,
            working_dir: spec.workdir.clone(),
            user: spec.user.clone(),
            hostname: spec.hostname.clone(),
            stop_signal: spec.stop_signal.clone(),
            labels,
        },
        image: None,
        rootfs_ref: info.rootfs_ref.clone(),
        host_config: InspectHostConfigJson {
            port_bindings: info
                .ports
                .iter()
                .map(|&(host, container)| PortBindingJson { host, container })
                .collect(),
            restart_policy: restart_policy(spec.restart.as_deref()),
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
//
// Split out via `#[path]` to keep this file under the 400-line godfile cap
// (house convention — see oci.rs / network.rs).

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
