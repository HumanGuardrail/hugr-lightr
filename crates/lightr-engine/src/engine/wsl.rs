//! WslEngine — Windows isolation via WSL2 (the Windows analog of Vz/Ns).
//!
//! The Windows isolation engine. It is the Windows analog of `vz` (macOS) and
//! `ns` (Linux): isolation is provided by running the workload inside the WSL2
//! utility VM, then applying the SAME `ns` model (Linux user/mount/pid
//! namespaces + pivot_root) *inside* that distro. We do NOT run a daemon — the
//! WSL2 utility VM is the OS's own lightweight, on-demand Hyper-V VM, managed by
//! Windows, not by us; `wsl.exe` is a transient launcher. "No daemon" holds.
//!
//! Future ring (named, NOT built here): a Hyper-V microVM engine (the Windows
//! vz-analog) that boots our own kernel+initrd pack directly via the Host Compute
//! System / Hyper-V, bypassing WSL. That is a separate, later engine.

use super::spec::ExecSpec;
use super::Engine;
// Used only by the non-Windows stub below; the Windows `wsl_impl` has its own.
#[cfg(not(target_os = "windows"))]
use lightr_core::{LightrError, Result};

// ── WslEngine (Windows) ───────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod wsl_impl {
    use super::{Engine, ExecSpec};
    use crate::engine::native::exit_code;
    use lightr_core::{LightrError, Result};

    pub struct WslEngine;

    impl Engine for WslEngine {
        /// Run the workload inside the default WSL2 distro and return its REAL
        /// exit code. The exit-code/signal law is the shared `exit_code`
        /// helper: `wsl.exe` propagates the in-distro process's exit status,
        /// and on Windows `ExitStatus::code()` carries it through.
        fn run(&self, spec: &ExecSpec) -> Result<i32> {
            // WIN-PATH: real execution, validatable only on a Windows box with
            // WSL2 + a registered distro. Probe (probe_wsl) gates `engine_for`
            // before we get here, so reaching this with no WSL is not expected;
            // we still fail closed if `wsl.exe` cannot be spawned.
            let (prog, args) = spec
                .command
                .split_first()
                .ok_or_else(|| LightrError::InvalidRef("empty command".to_string()))?;

            let mut cmd = std::process::Command::new("wsl.exe");

            // Run in the user's default distro. `--cd` sets the working
            // directory *inside* the distro (a Linux path); fall back to the
            // distro default when cwd is unusable as a Linux path.
            if let Some(cwd) = spec.cwd.to_str() {
                if cwd.starts_with('/') {
                    cmd.args(["--cd", cwd]);
                }
            }

            match spec.rootfs {
                // Isolated run: reuse the REAL `ns` engine INSIDE the distro.
                // There is no external shim — the `ns` model is in-process in
                // `NsEngine::run` (unshare + pivot_root) — so we invoke a Linux
                // `lightr` on the distro PATH with `run --engine ns --rootfs …`,
                // which runs that same in-process ns path inside WSL2. The rootfs
                // is a host (Windows) path; translate it to the WSL2 mount view
                // (C:\x -> /mnt/c/x) so the in-distro process can see it.
                //
                // WIN-PATH: requires a Linux `lightr` installed in the default
                // WSL2 distro; runtime-validated via the Windows runbook, not here.
                Some(rootfs) => {
                    let rootfs_str = rootfs.to_str().ok_or_else(|| {
                        LightrError::InvalidRef(
                            "wsl engine: rootfs path is not valid UTF-8 for in-distro use"
                                .to_string(),
                        )
                    })?;
                    let wsl_rootfs = win_path_to_wsl(rootfs_str);
                    // `--` ends wsl.exe option parsing; everything after runs in
                    // the distro. Reuse the real ns engine via the public CLI.
                    cmd.arg("--");
                    cmd.args(["lightr", "run", "--engine", "ns", "--rootfs"]);
                    cmd.arg(&wsl_rootfs);
                    cmd.arg("--");
                    cmd.arg(prog);
                    cmd.args(args);
                }
                // No rootfs: run the command directly in the distro (the WSL2
                // VM is still the isolation boundary vs. the Windows host).
                None => {
                    cmd.arg("--");
                    cmd.arg(prog);
                    cmd.args(args);
                }
            }

            // Inherit stdio (stdout/stderr passed through), like NativeEngine.
            let status = cmd.status().map_err(LightrError::Io)?;
            Ok(exit_code(status))
        }
    }

    /// Translate a Windows path (`C:\a\b`) to its WSL2 mount view (`/mnt/c/a/b`)
    /// so an in-distro process can see a host-materialized rootfs. Already-Linux
    /// paths pass through. WIN-PATH: covers drive-backed paths (the common case
    /// for a materialized rootfs); validated end-to-end via the Windows runbook.
    fn win_path_to_wsl(p: &str) -> String {
        if p.starts_with('/') {
            return p.to_string();
        }
        let bytes = p.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            let drive = (bytes[0] as char).to_ascii_lowercase();
            let rest = p[2..].replace('\\', "/");
            return format!("/mnt/{drive}{rest}");
        }
        p.replace('\\', "/")
    }
}

#[cfg(target_os = "windows")]
pub(super) fn wsl_engine_box() -> Box<dyn Engine> {
    Box::new(wsl_impl::WslEngine)
}

/// Stub for non-Windows builds so `engine_for` can name the `Wsl` arm. The
/// probe (`probe_wsl`) gates before this is ever constructed in production, so
/// this path is dead-code on unix in practice — it just keeps the match total.
#[cfg(not(target_os = "windows"))]
struct WslEngineStub;

#[cfg(not(target_os = "windows"))]
impl Engine for WslEngineStub {
    fn run(&self, _spec: &ExecSpec) -> Result<i32> {
        Err(LightrError::InvalidRef(
            "wsl engine requires Windows + WSL2".to_string(),
        ))
    }
}

#[cfg(not(target_os = "windows"))]
pub(super) fn wsl_engine_box() -> Box<dyn Engine> {
    Box::new(WslEngineStub)
}
