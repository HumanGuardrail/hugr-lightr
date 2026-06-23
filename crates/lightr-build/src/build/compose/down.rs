//! compose_down: tear down a compose stack.
use lightr_core::{LightrError, Result};
use std::path::{Path, PathBuf};

use super::model::{ServiceSpec, StackSpec};

/// #75 FIX-1: every run dir recorded for a service, de-duplicated and in record
/// order. Folds the current `run_dirs` list (one entry per replica instance) with
/// the legacy scalar `run_dir` (pre-fix stacks) so `compose down` stops them all.
fn recorded_run_dirs(svc: &ServiceSpec) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(svc.run_dirs.len() + 1);
    for d in svc.run_dirs.iter().chain(svc.run_dir.iter()) {
        if !out.contains(d) {
            out.push(d.clone());
        }
    }
    out
}

/// Tear down a compose stack.
///
/// 1. Reads `spec.json` and stops any started service runs.
/// 2. Writes `stop` file to signal the supervisor.
/// 3. Removes the stack directory.
pub fn compose_down(stack_dir: &Path) -> Result<()> {
    let spec_path = stack_dir.join("spec.json");
    if spec_path.exists() {
        if let Ok(bytes) = std::fs::read(&spec_path) {
            if let Ok(spec) = serde_json::from_slice::<StackSpec>(&bytes) {
                for svc in &spec.services {
                    // #75 FIX-1: stop EVERY recorded instance. `deploy.replicas: N`
                    // records N run dirs in `run_dirs`; the pre-fix single field
                    // stopped only one, orphaning the other N-1 forever. The legacy
                    // scalar `run_dir` is folded in so a stack `up`'d before this fix
                    // still tears down. Distinct paths only (a replica recorded once).
                    for run_dir in recorded_run_dirs(svc) {
                        let dir = PathBuf::from(run_dir);
                        if dir.exists() {
                            let _ = lightr_run::stop(&dir, 2);
                        }
                    }
                }
            }
        }
    }

    let stop_file = stack_dir.join("stop");
    let _ = std::fs::write(&stop_file, b"");

    #[cfg(unix)]
    {
        let pid_file = stack_dir.join("pid");
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
    }

    if stack_dir.exists() {
        std::fs::remove_dir_all(stack_dir).map_err(LightrError::Io)?;
    }

    Ok(())
}

#[cfg(test)]
#[path = "down_tests.rs"]
mod tests;
