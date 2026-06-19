//! `lightr ps` handler — list running/exited run instances.

use lightr_run::ps;
use serde::Serialize;

use crate::{exit::die_lightr, lightr_home};

/// Port mapping serialized as `{ "host": u16, "container": u16 }` for
/// readability in `--json` output (mirrors Docker's `Ports` field shape).
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
    /// `None` when no healthcheck was configured.
    health: Option<String>,
    /// Engine that runs this job: "native" or "vz".
    engine: String,
    /// Published port mappings (may be empty).
    ports: Vec<PortMapJson>,
    /// Rootfs ref for vz runs; `null` for native runs.
    rootfs_ref: Option<String>,
}

pub fn run(json: bool) -> i32 {
    let home = lightr_home();

    let runs = match ps(&home) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    if json {
        let arr: Vec<RunInfoJson> = runs
            .iter()
            .map(|r| RunInfoJson {
                id: r.id.clone(),
                running: r.running,
                exit_code: r.exit_code,
                command: r.command.clone(),
                created_at_unix: r.created_at_unix,
                health: r.health.map(|h| h.as_str().to_string()),
                engine: r.engine.clone(),
                ports: r
                    .ports
                    .iter()
                    .map(|&(host, container)| PortMapJson { host, container })
                    .collect(),
                rootfs_ref: r.rootfs_ref.clone(),
            })
            .collect();
        println!("{}", serde_json::to_string(&arr).expect("serialize ps"));
    } else {
        for r in &runs {
            let status = if r.running {
                "running".to_string()
            } else {
                format!("exited {}", r.exit_code.unwrap_or(0))
            };
            let cmd0 = r.command.first().map(|s| s.as_str()).unwrap_or("<none>");

            // Health suffix: only shown when a healthcheck is configured.
            let health_suffix = match r.health {
                Some(h) => format!("  [{}]", h.as_str()),
                None => String::new(),
            };

            // Engine/rootfs suffix: only shown when non-native or rootfs is set.
            let engine_suffix = if r.engine != "native" || r.rootfs_ref.is_some() {
                let rootfs = r
                    .rootfs_ref
                    .as_deref()
                    .map(|s| format!("/{s}"))
                    .unwrap_or_default();
                format!("  [{}{}]", r.engine, rootfs)
            } else {
                String::new()
            };

            // Ports suffix: compact `host:container[,...]` notation.
            let ports_suffix = if r.ports.is_empty() {
                String::new()
            } else {
                let pairs: Vec<String> = r.ports.iter().map(|&(h, c)| format!("{h}:{c}")).collect();
                format!("  [{}]", pairs.join(","))
            };

            println!(
                "{:<24}  {:<16}  {}{}{}{}",
                r.id, status, cmd0, engine_suffix, ports_suffix, health_suffix
            );
        }
    }

    0
}
