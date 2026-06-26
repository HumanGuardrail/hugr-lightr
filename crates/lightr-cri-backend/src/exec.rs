//! exec plane (sync) + container stats (WP-CRI-MVP).
//!
//! PROVENANCE: `exec_sync` is TRANSCRIBED from the conformance reference
//! `lightr-cri-fake::exec_sync` — run the command in the container's execution
//! context (cwd + env), capture stdout/stderr, map the exit code (128+sig on
//! signal), and honor a timeout by SIGKILLing the child (and its process group)
//! then reaping it so no `sleep` survives critest's leftover-process check.
//!
//! REUSE NOTE: `lightr_run::exec_in` is NOT used — it inherits stdio
//! (passthrough, no capture) and has no timeout, whereas CRI exec_sync REQUIRES
//! captured stdout/stderr/exit and a deadline. We mirror the fake's captured
//! spawn instead. Joining the container's namespaces (setns) is WP-CRI-SANDBOX.

use crate::stats::read_proc_stats;
use crate::util::{exit_code_from_status, now_nanos, ContainerRecord};
use crate::vocab::{
    BackendError, ContainerFilter, ContainerId, ContainerState, ContainerStatsRec, ExecResult,
    Result,
};
use crate::LightrBackend;

impl LightrBackend {
    pub(crate) fn exec_sync_impl(
        &self,
        id: &ContainerId,
        cmd: &[String],
        timeout_seconds: i64,
    ) -> Result<ExecResult> {
        let rec = self
            .cache
            .lock()
            .unwrap()
            .containers
            .get(&id.0)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;

        if rec.state != ContainerState::Running {
            return Err(BackendError::FailedPrecondition(format!(
                "container {} is not Running (state={:?}); exec_sync requires Running",
                id.0, rec.state
            )));
        }
        if cmd.is_empty() {
            return Err(BackendError::InvalidArgument(
                "exec_sync: empty command".to_string(),
            ));
        }

        // WP-#100 (exec slice 1): for an `ns` container, ENTER it via the
        // `__ns-exec` re-exec shim (setns into PID-1's namespaces) instead of
        // spawning a host process. Fail-closed: if the PID 1 cannot be resolved we
        // return the error — NEVER a host exec (that would run OUTSIDE the
        // container = false). Every other case (rec.engine != "ns", all non-linux)
        // keeps today's exact host-process behavior (behavior-preserving: the
        // host_network sandboxes, the conformance/vector tests, the macOS gate).
        #[cfg(target_os = "linux")]
        let mut command = if rec.engine == "ns" {
            self.ns_exec_command(&rec, cmd)?
        } else {
            host_exec_command(&rec, cmd)
        };
        #[cfg(not(target_os = "linux"))]
        let mut command = host_exec_command(&rec, cmd);

        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        // Own process group so a timeout can SIGKILL the whole tree (transcribed
        // from the fake: critest wraps a forking shell; killing only the
        // immediate child would leave a grandchild holding the stdout pipe).
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command
            .spawn()
            .map_err(|e| BackendError::Internal(format!("exec_sync spawn: {e}")))?;

        if timeout_seconds > 0 {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds as u64);
            loop {
                match child
                    .try_wait()
                    .map_err(|e| BackendError::Internal(format!("try_wait: {e}")))?
                {
                    Some(status) => {
                        let stdout = read_child(&mut child, true);
                        let stderr = read_child(&mut child, false);
                        return Ok(ExecResult {
                            exit_code: exit_code_from_status(&status),
                            stdout,
                            stderr,
                        });
                    }
                    None => {
                        if std::time::Instant::now() >= deadline {
                            // Kill the child PID directly AND its negative pgid to
                            // sweep grandchildren, then blocking-reap so nothing
                            // survives (transcribed from the fake's timeout path).
                            #[cfg(unix)]
                            unsafe {
                                let pid = child.id() as i32;
                                libc::kill(pid, libc::SIGKILL);
                                libc::kill(-pid, libc::SIGKILL);
                            }
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(BackendError::Internal("exec timeout".to_string()));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }
        } else {
            let output = child
                .wait_with_output()
                .map_err(|e| BackendError::Internal(format!("exec_sync wait: {e}")))?;
            Ok(ExecResult {
                exit_code: exit_code_from_status(&output.status),
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
    }

    // ── stats ──────────────────────────────────────────────────────────────

    pub(crate) fn container_stats_impl(&self, id: &ContainerId) -> Result<ContainerStatsRec> {
        let rec = self
            .cache
            .lock()
            .unwrap()
            .containers
            .get(&id.0)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;

        let ts = now_nanos();
        // Not Running / no pid ⇒ probe-truthful zero with a real timestamp.
        if rec.state != ContainerState::Running || rec.pid == 0 {
            return Ok(ContainerStatsRec {
                id: id.clone(),
                timestamp_nanos: ts,
                cpu_usage_core_nanos: 0,
                memory_working_set_bytes: 0,
            });
        }
        let (cpu, mem) = read_proc_stats(rec.pid);
        Ok(ContainerStatsRec {
            id: id.clone(),
            timestamp_nanos: ts,
            cpu_usage_core_nanos: cpu,
            memory_working_set_bytes: mem,
        })
    }

    pub(crate) fn list_container_stats_impl(
        &self,
        filter: &ContainerFilter,
    ) -> Result<Vec<ContainerStatsRec>> {
        let ids: Vec<ContainerId> = {
            let cache = self.cache.lock().unwrap();
            cache
                .containers
                .values()
                .filter(|r| crate::util::container_matches(r, filter))
                .map(|r| r.id.clone())
                .collect()
        };
        ids.iter().map(|id| self.container_stats_impl(id)).collect()
    }
}

impl LightrBackend {
    /// WP-#100: build the `__ns-exec` re-exec Command that ENTERS the `ns`
    /// container (setns into PID-1's namespaces). Resolves the container's
    /// in-pidns PID 1 from its cgroup, serializes an [`ExecDescriptor`], and
    /// hands it to the shim via `LIGHTR_NSEXEC_DESC`. The shim execve's with the
    /// descriptor's env, so `LIGHTR_NSEXEC_DESC` + the serve's env never leak
    /// inside. Shared with `open_exec_impl` (the pipe path). Linux-only — the ns
    /// path is only ever taken on linux.
    #[cfg(target_os = "linux")]
    pub(crate) fn ns_exec_command(
        &self,
        rec: &ContainerRecord,
        cmd: &[String],
    ) -> Result<std::process::Command> {
        use crate::ns_exec::ExecDescriptor;
        let pid1 = self.container_pid1(&rec.cgroup_name)?;
        let desc = ExecDescriptor {
            pid1,
            argv: cmd.to_vec(),
            cwd: rec.config.working_dir.clone(),
            env: rec.config.envs.clone(),
            tty: false,
        };
        let json = serde_json::to_string(&desc)
            .map_err(|e| BackendError::Internal(format!("serialize exec descriptor: {e}")))?;
        let exe = std::env::current_exe()
            .map_err(|e| BackendError::Internal(format!("current_exe: {e}")))?;
        let mut command = std::process::Command::new(exe);
        command.arg("__ns-exec");
        command.env("LIGHTR_NSEXEC_DESC", json);
        Ok(command)
    }
}

/// Build the host-process exec Command (today's behavior): run `cmd` in the
/// container's cwd+env on the HOST namespaces. Used for non-`ns` containers
/// (host_network) and on every non-linux build. Behavior-preserving.
fn host_exec_command(rec: &ContainerRecord, cmd: &[String]) -> std::process::Command {
    let mut command = std::process::Command::new(&cmd[0]);
    command.args(&cmd[1..]);
    if !rec.config.working_dir.is_empty() {
        command.current_dir(&rec.config.working_dir);
    }
    for (k, v) in &rec.config.envs {
        command.env(k, v);
    }
    command
}

/// Drain a finished child's stdout/stderr pipe to bytes (transcribed from the
/// fake's `read_child_output`). Used only on the timeout-loop terminal branch
/// where `wait_with_output` is unavailable (the status came from `try_wait`).
fn read_child(child: &mut std::process::Child, stdout: bool) -> Vec<u8> {
    use std::io::Read;
    let mut buf = Vec::new();
    if stdout {
        if let Some(mut out) = child.stdout.take() {
            let _ = out.read_to_end(&mut buf);
        }
    } else if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut buf);
    }
    buf
}
