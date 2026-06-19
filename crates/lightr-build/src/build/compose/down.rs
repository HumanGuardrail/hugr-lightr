//! compose_down: tear down a compose stack.
use lightr_core::{LightrError, Result};
use std::path::{Path, PathBuf};

use super::model::StackSpec;

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
                    if let Some(run_dir) = &svc.run_dir {
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn compose_down_nonexistent_is_ok() {
        let tmp = TempDir::new().unwrap();
        let fake = tmp.path().join("no-such-stack");
        assert!(compose_down(&fake).is_ok());
    }
}
