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
        // WP-IMG-ENVUSER: consume the image's recorded ENV + USER (merged with the
        // CLI's `-e`/`-u` by the run handler — CLI wins). Empty env + None user ⇒
        // no-op, so a config-less image / no-flag run is byte-identical to before.
        super::envuser::apply_env(&mut cmd, spec.env);
        super::envuser::apply_user(&mut cmd, spec.user)?;
        // F-203: apply resource caps. On Linux: installs a pre_exec RLIMIT_AS/
        // RLIMIT_DATA hook for memory_bytes; cpu_millis is unsupported on native
        // (returns honest Err). No-op when limits are unlimited; Err on macOS cap.
        crate::limits::apply_native(&mut cmd, &spec.limits)?;
        // `--ulimit`: install a `pre_exec` `setrlimit` hook for each requested
        // per-process limit (the same idiom as `apply_native`'s memory RLIMIT
        // hook). Empty ⇒ no hook installed (byte-identical to before). Fail-closed:
        // a failing `setrlimit` (e.g. a rootless hard-limit raise ⇒ EPERM) aborts
        // the exec from the pre_exec closure (the spawn surfaces the io::Error).
        crate::limits::apply_native_ulimits(&mut cmd, spec.ulimits);
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
