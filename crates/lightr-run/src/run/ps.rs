//! ps — list all detached runs with status enrichment.

use lightr_core::{LightrError, Result};

use super::ctl::ctl_sock_path;
use super::paths::{
    parse_exit_code_from_status, pid_alive, read_pid_file, read_spec_on_disk, read_status_file,
};
use super::types::RunInfo;

pub fn ps(store_home: &std::path::Path) -> Result<Vec<RunInfo>> {
    let run_dir = store_home.join("run");

    if !run_dir.exists() {
        return Ok(vec![]);
    }

    let mut infos: Vec<RunInfo> = Vec::new();

    let entries = std::fs::read_dir(&run_dir).map_err(LightrError::Io)?;
    for entry in entries {
        let entry = entry.map_err(LightrError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Read spec.json
        let spec = match read_spec_on_disk(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Determine running state
        let sock = ctl_sock_path(&path);
        let running = if sock.exists() {
            // Also check pid alive
            if let Some(pid) = read_pid_file(&path) {
                #[cfg(any(unix, windows))]
                {
                    pid_alive(pid)
                }
                #[cfg(not(any(unix, windows)))]
                {
                    true
                }
            } else {
                false
            }
        } else {
            false
        };

        let exit_code = read_status_file(&path)
            .as_deref()
            .and_then(parse_exit_code_from_status);

        let health = crate::healthcheck::read_state(&path);

        infos.push(RunInfo {
            id,
            running,
            exit_code,
            command: spec.command,
            created_at_unix: spec.created_at_unix,
            health,
            engine: spec.engine,
            ports: spec.ports,
            rootfs_ref: spec.rootfs_ref,
        });
    }

    // Sort by id descending (newest first — id starts with unix_nanos)
    infos.sort_by(|a, b| b.id.cmp(&a.id));

    Ok(infos)
}
