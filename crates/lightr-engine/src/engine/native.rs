//! NativeEngine — direct process execution with no isolation.

use super::spec::ExecSpec;
use super::Engine;
use lightr_core::{LightrError, Result};

// ── NativeEngine ──────────────────────────────────────────────────────────────

pub struct NativeEngine;

impl Engine for NativeEngine {
    fn run(&self, spec: &ExecSpec) -> Result<i32> {
        if spec.rootfs.is_some() {
            return Err(LightrError::InvalidRef(
                "native engine has no rootfs".to_string(),
            ));
        }
        let (prog, args) = spec
            .command
            .split_first()
            .ok_or_else(|| LightrError::InvalidRef("empty command".to_string()))?;
        let mut cmd = std::process::Command::new(prog);
        cmd.args(args).current_dir(spec.cwd);
        // inherit all env from parent; inherit stdio (stdout/stderr passed through)
        // F-203: apply resource caps. A0 stub is Ok(()); WP-A1 fills it.
        crate::limits::apply_native(&mut cmd, &spec.limits)?;
        let status = cmd.status().map_err(LightrError::Io)?;
        Ok(exit_code(status))
    }
}

#[cfg(unix)]
pub(crate) fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        128 + sig
    } else {
        1
    }
}

#[cfg(not(unix))]
pub(crate) fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}
