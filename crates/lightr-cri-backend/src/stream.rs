//! Streaming plane — exec/attach over real OS stdio (WP-CRI-STREAM).
//!
//! PROVENANCE: TRANSCRIBED from the conformance reference
//! `lightr-cri/crates/lightr-cri-fake/src/lib.rs` (open_exec ~1601, open_attach
//! ~1771). The fake is what passes critest's kubectl-exec/attach round-trips;
//! we mirror its semantics. The supporting I/O machinery (io-table, fan-out,
//! waiters, fd primitives, the start_container hook) lives in `stream_io` —
//! split out to keep both files <400 LOC (godfile law).
//!
//! LEAD DECISION (transport): the backend returns real OS file handles in the
//! `StreamSession` directly — the fake's model, where the CRI shell drives the
//! handles. NOT a ctl.sock-sentinel indirection: this is an in-process backend,
//! so the shell holds the fds. (The handoff ADR-0017 keeps the seam wire-level;
//! a future out-of-process server fronts these fds, it does not change them.)
//!
//! LEAD DECISION (no `nix`): the fake uses `nix::pty::openpty`; this crate does
//! not depend on `nix`. We open the pty via `libc::openpty` (present on macOS +
//! Linux), keeping the dependency surface unchanged.
//!
//! - open_exec spawns `cmd` in the container's context (cwd + env, mirroring
//!   how container.rs spawns; netns-join is WP-CRI-SANDBOX's cfg(linux)
//!   concern), with piped (or pty when tty) stdio, and returns a real
//!   `ChildWaiter` that reaps the child once and yields 128+sig / code.
//! - open_attach registers a fresh fan-out pipe sink with the RUNNING
//!   container's tee (pipe mode) or dups the held pty master (tty mode); its
//!   waiter completes on container exit.
//! - Interactive output reaches the client immediately: the tee (in `stream_io`)
//!   is a single reader that broadcasts each raw chunk to every attacher BEFORE
//!   any line framing, so no `\n`-boundary buffer stall (the defect that hung
//!   the fake's attach round-trip).
//!
//! cfg(unix): pty, OS pipes, and signal handling are unix concepts. The windows
//! gate is build+clippy only — the `open_*_impl` entry points fail closed
//! honestly on non-unix (template 8a).

use crate::vocab::{BackendError, ContainerId, Result, StreamSession};
use crate::LightrBackend;

#[cfg(unix)]
impl LightrBackend {
    /// Open an exec session: spawn `cmd` in the container's execution context
    /// (cwd + env, mirroring container.rs), piped or pty stdio, real waiter.
    pub(crate) fn open_exec_impl(
        &self,
        id: &ContainerId,
        cmd: &[String],
        tty: bool,
        stdin: bool,
    ) -> Result<StreamSession> {
        use crate::vocab::ContainerState;
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
                "container {} is not Running (state={:?}); open_exec requires Running",
                id.0, rec.state
            )));
        }
        if cmd.is_empty() {
            return Err(BackendError::InvalidArgument(
                "open_exec: empty command".to_string(),
            ));
        }

        let mut command = std::process::Command::new(&cmd[0]);
        command.args(&cmd[1..]);
        if !rec.config.working_dir.is_empty() {
            command.current_dir(&rec.config.working_dir);
        }
        for (k, v) in &rec.config.envs {
            command.env(k, v);
        }

        // WP-#100 (slice 1) + WP-#103 (slice 2): for an `ns` container, exec must
        // ENTER the container via the `__ns-exec` re-exec shim (setns into PID-1's
        // namespaces) — same nsenter model as `exec_sync`. This now covers BOTH the
        // non-tty (pipe) path AND the tty path: `ns_exec_command(.., tty)` carries
        // `tty` in the descriptor so the workload grandchild does setsid/TIOCSCTTY
        // on its pty slave inside the container. The only difference between tty and
        // non-tty is HOW this Command's stdio is wired below (pty vs pipes) — the
        // Command itself is `__ns-exec` either way. Fail-closed: a PID-1 resolution
        // error returns, never a host exec for an ns container. Non-`ns` containers
        // (host_network) and every non-linux build keep today's exact host-process
        // behavior (the bare command runs on the host namespaces).
        #[cfg(target_os = "linux")]
        if rec.engine == "ns" {
            command = self.ns_exec_command(&rec, cmd, tty)?;
        }

        if tty {
            open_exec_tty(command)
        } else {
            open_exec_pipe(command, stdin)
        }
    }

    /// Attach to the RUNNING container's live stdio via the held io-table entry.
    /// tty: dup the pty master into stdout + pty_master. Pipe: register a fresh
    /// fan-out pipe sink per stream (read-end handed back), dup the stdin
    /// write-end if held. Waiter completes on container exit.
    pub(crate) fn open_attach_impl(&self, id: &ContainerId) -> Result<StreamSession> {
        use crate::stream_io::{dup_file, AttachWaiter};
        use crate::vocab::{ContainerState, ExitWaiter};
        use std::sync::Arc;
        {
            let cache = self.cache.lock().unwrap();
            let rec = cache
                .containers
                .get(&id.0)
                .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;
            if rec.state != ContainerState::Running {
                return Err(BackendError::FailedPrecondition(format!(
                    "container {} is not Running (state={:?}); open_attach requires Running",
                    id.0, rec.state
                )));
            }
        }

        let io = self.io_table.lock().unwrap();
        let entry = io.get(&id.0).ok_or_else(|| {
            BackendError::Internal("attach unavailable after restart".to_string())
        })?;

        let waiter: Box<dyn ExitWaiter> = Box::new(AttachWaiter {
            cache: Arc::clone(&self.cache),
            id: id.clone(),
        });

        if let Some(master) = &entry.pty_master {
            let stdout = dup_file(master).map_err(BackendError::Io)?;
            let pty_master = dup_file(master).map_err(BackendError::Io)?;
            return Ok(StreamSession {
                stdin: None,
                stdout: Some(stdout),
                stderr: None,
                pty_master: Some(pty_master),
                waiter,
            });
        }

        // pipe mode: register fresh fan-out sinks; the tee copies the
        // container's raw bytes into our pipes in addition to the CRI log —
        // no second reader, no race with the log tee.
        let fanout = entry.fanout.clone().ok_or_else(|| {
            BackendError::Internal("pipe-mode container has no output fan-out".to_string())
        })?;
        let stdout_attach = if entry.has_stdout {
            Some(fanout.register("stdout").map_err(BackendError::Io)?)
        } else {
            None
        };
        let stderr_attach = if entry.has_stderr {
            Some(fanout.register("stderr").map_err(BackendError::Io)?)
        } else {
            None
        };
        let stdin_attach = match &entry.stdin_wr {
            Some(w) => Some(dup_file(w).map_err(BackendError::Io)?),
            None => None,
        };
        drop(io);

        Ok(StreamSession {
            stdin: stdin_attach,
            stdout: stdout_attach,
            stderr: stderr_attach,
            pty_master: None,
            waiter,
        })
    }
}

/// tty=true: child stdio = pty slave; stdout = pty master clone, pty_master =
/// master, stderr None. setsid in pre_exec so the child owns the pty as its
/// controlling terminal. Transcribed from the fake.
#[cfg(unix)]
fn open_exec_tty(mut command: std::process::Command) -> Result<StreamSession> {
    use crate::stream_io::{dup_file, open_pty, ChildWaiter};
    let (master_file, slave_file) = open_pty().map_err(BackendError::Io)?;
    let slave_stdin = dup_file(&slave_file).map_err(BackendError::Io)?;
    let slave_stdout = dup_file(&slave_file).map_err(BackendError::Io)?;
    let slave_stderr = slave_file; // last use — move it

    use std::os::unix::process::CommandExt;
    command.stdin(slave_stdin);
    command.stdout(slave_stdout);
    command.stderr(slave_stderr);
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let child = command
        .spawn()
        .map_err(|e| BackendError::Internal(format!("open_exec spawn: {e}")))?;

    let stdout_fd = dup_file(&master_file).map_err(BackendError::Io)?;
    Ok(StreamSession {
        stdin: None, // tty: write to the master, no separate stdin pipe
        stdout: Some(stdout_fd),
        stderr: None,
        pty_master: Some(master_file),
        waiter: Box::new(ChildWaiter { child }),
    })
}

/// pipe-mode exec: piped stdout/stderr (and stdin when requested); the
/// StreamSession carries the read-ends of stdout/stderr and the write-end of
/// stdin. Transcribed from the fake's non-tty branch.
#[cfg(unix)]
fn open_exec_pipe(mut command: std::process::Command, stdin: bool) -> Result<StreamSession> {
    use crate::stream_io::ChildWaiter;
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    use std::process::Stdio;
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if stdin {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }

    let mut child = command
        .spawn()
        .map_err(|e| BackendError::Internal(format!("open_exec spawn: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .map(|s| unsafe { std::fs::File::from_raw_fd(s.into_raw_fd()) });
    let stderr = child
        .stderr
        .take()
        .map(|s| unsafe { std::fs::File::from_raw_fd(s.into_raw_fd()) });
    let stdin_file = child
        .stdin
        .take()
        .map(|s| unsafe { std::fs::File::from_raw_fd(s.into_raw_fd()) });

    Ok(StreamSession {
        stdin: stdin_file,
        stdout,
        stderr,
        pty_master: None,
        waiter: Box::new(ChildWaiter { child }),
    })
}

// ── non-unix fail-closed (windows gate compiles, never runs) ─────────────────

#[cfg(not(unix))]
impl LightrBackend {
    pub(crate) fn open_exec_impl(
        &self,
        _id: &ContainerId,
        _cmd: &[String],
        _tty: bool,
        _stdin: bool,
    ) -> Result<StreamSession> {
        Err(BackendError::Internal(
            "open_exec: streaming plane is unix-only".to_string(),
        ))
    }
    pub(crate) fn open_attach_impl(&self, _id: &ContainerId) -> Result<StreamSession> {
        Err(BackendError::Internal(
            "open_attach: streaming plane is unix-only".to_string(),
        ))
    }

    /// Log-only tee for the non-unix build (no io-table / fan-out: attach is
    /// unix-only). Mirrors the unix `register_io_and_tee` call site in
    /// `container.rs` so `start_container` is platform-uniform.
    pub(crate) fn register_io_and_tee(
        &self,
        _id: &ContainerId,
        child: &mut std::process::Child,
        log_shared: &std::sync::Arc<std::sync::Mutex<Option<std::fs::File>>>,
    ) {
        use crate::util::spawn_tee_thread;
        use std::sync::Arc;
        if let Some(out) = child.stdout.take() {
            spawn_tee_thread("stdout", out, Arc::clone(log_shared));
        }
        if let Some(err) = child.stderr.take() {
            spawn_tee_thread("stderr", err, Arc::clone(log_shared));
        }
    }
}

#[cfg(all(test, unix))]
#[path = "stream_tests.rs"]
mod tests;
