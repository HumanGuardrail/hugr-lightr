//! logs — read or follow stdout/stderr log files for a detached run.

use lightr_core::{LightrError, Result};

use super::paths::read_status_file;
use super::types::LogStream;

pub fn logs(dir: &std::path::Path, stream: LogStream, follow: bool) -> Result<()> {
    use std::io::Write;

    fn print_file(path: &std::path::Path, offset: &mut u64) -> Result<bool> {
        let data = std::fs::read(path).map_err(LightrError::Io)?;
        let start = *offset as usize;
        if start < data.len() {
            std::io::stdout()
                .write_all(&data[start..])
                .map_err(LightrError::Io)?;
            *offset = data.len() as u64;
            return Ok(true);
        }
        Ok(false)
    }

    let stdout_path = dir.join("stdout.log");
    let stderr_path = dir.join("stderr.log");

    if !follow {
        match stream {
            LogStream::Stdout => {
                let _ = print_file(&stdout_path, &mut 0u64);
            }
            LogStream::Stderr => {
                let _ = print_file(&stderr_path, &mut 0u64);
            }
            LogStream::Both => {
                let _ = print_file(&stdout_path, &mut 0u64);
                let _ = print_file(&stderr_path, &mut 0u64);
            }
        }
        return Ok(());
    }

    // Follow mode
    let mut stdout_off = 0u64;
    let mut stderr_off = 0u64;

    loop {
        let mut had_new = false;
        match stream {
            LogStream::Stdout => {
                if stdout_path.exists() {
                    had_new |= print_file(&stdout_path, &mut stdout_off)?;
                }
            }
            LogStream::Stderr => {
                if stderr_path.exists() {
                    had_new |= print_file(&stderr_path, &mut stderr_off)?;
                }
            }
            LogStream::Both => {
                if stdout_path.exists() {
                    had_new |= print_file(&stdout_path, &mut stdout_off)?;
                }
                if stderr_path.exists() {
                    had_new |= print_file(&stderr_path, &mut stderr_off)?;
                }
            }
        }
        let _ = had_new;

        // Check if exited and no new bytes
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            // Drain any remaining
            let mut drained = false;
            match stream {
                LogStream::Stdout => {
                    if stdout_path.exists() {
                        drained |= print_file(&stdout_path, &mut stdout_off)?;
                    }
                }
                LogStream::Stderr => {
                    if stderr_path.exists() {
                        drained |= print_file(&stderr_path, &mut stderr_off)?;
                    }
                }
                LogStream::Both => {
                    if stdout_path.exists() {
                        drained |= print_file(&stdout_path, &mut stdout_off)?;
                    }
                    if stderr_path.exists() {
                        drained |= print_file(&stderr_path, &mut stderr_off)?;
                    }
                }
            }
            if !drained {
                break;
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    Ok(())
}
