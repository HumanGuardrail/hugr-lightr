//! exec_in — execute a command inside an existing run's cwd.

use lightr_core::{LightrError, Result};
use std::path::PathBuf;

use super::paths::read_spec_on_disk;

pub fn exec_in(dir: &std::path::Path, command: &[String]) -> Result<i32> {
    let spec = read_spec_on_disk(dir)?;
    let cwd = PathBuf::from(&spec.cwd);

    if command.is_empty() {
        return Err(LightrError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command is empty",
        )));
    }

    let mut child = std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(LightrError::Io)?;

    let status = child.wait().map_err(LightrError::Io)?;

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }
    #[cfg(not(unix))]
    {
        Ok(status.code().unwrap_or(1))
    }
}
