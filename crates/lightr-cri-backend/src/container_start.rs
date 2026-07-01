//! Container start — the spawn/readiness/reaper core flow (WP-CRI-MVP).
//!
//! Extracted from `container.rs` (behavior-preserving split): `start_container_impl`
//! is the single largest lifecycle operation (host path + `ns` path + the WP-#102
//! exec-readiness wait + the detached reaper). It delegates to `build_ns_plan`
//! (container_setup.rs) and `wait_exec_ready` (container_wait.rs); the rest of the
//! lifecycle (create/stop/remove/cgroup) stays in `container.rs`.

use std::fs;
use std::sync::{Arc, Mutex};

use crate::util::{atomic_write_json, now_nanos, open_cri_log, signal_or_code};
use crate::vocab::{BackendError, ContainerId, ContainerState, Result};
use crate::LightrBackend;

impl LightrBackend {
    // ── start ────────────────────────────────────────────────────────────────

    pub(crate) fn start_container_impl(&self, id: &ContainerId) -> Result<()> {
        let rec = self
            .cache()
            .containers
            .get(&id.0)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;

        if rec.state != ContainerState::Created {
            return Err(BackendError::FailedPrecondition(format!(
                "container {} is in state {:?}, must be Created to start",
                id.0, rec.state
            )));
        }

        // sandbox log_directory (read from the persisted sandbox record).
        let sandbox_log_dir = self.cache().sandbox_log_dir(&rec.sandbox);

        // Open (create) the CRI log so the empty file exists from start (§C).
        let log = open_cri_log(&sandbox_log_dir, &rec.config.log_path).map_err(BackendError::Io)?;
        let log_shared: Arc<Mutex<Option<fs::File>>> = Arc::new(Mutex::new(log));

        // Build the argv. Empty command/args ⇒ keep-alive `tail -f /dev/null`
        // (transcribed from the fake: critest synthetic images carry no
        // entrypoint, the container must stay Running for exec).
        let argv: Vec<String> = if rec.config.command.is_empty() && rec.config.args.is_empty() {
            vec![
                "tail".to_string(),
                "-f".to_string(),
                "/dev/null".to_string(),
            ]
        } else {
            rec.config
                .command
                .iter()
                .chain(rec.config.args.iter())
                .cloned()
                .collect()
        };
        let program = argv
            .first()
            .cloned()
            .ok_or_else(|| BackendError::InvalidArgument("empty command".to_string()))?;

        // WP-#99 (CRI slice 1): decide the execution path. The NS path runs the
        // REAL image rootfs under the `ns` engine, joined into the pod's netns,
        // by re-exec'ing THIS binary as `__ns-run` with a `RunDescriptor` in the
        // LIGHTR_NSRUN_DESC env (off stdin, so the workload's stdin is free for
        // attach). It is taken ONLY when (linux + the pod has a pinned netns + the
        // ns engine is available + the image hydrates). EVERY other case falls
        // back to today's host-process path (behavior-preserving) — `ns_descriptor`
        // is `None` there, including on non-linux (so the macOS gate is untouched).
        // AUDIT FIX (#99): gate on whether the POD expects isolation (has a pinned
        // netns from CNI). If it does, the ns plan MUST succeed — a hydrate/engine
        // failure FAILS the start (`?`) rather than silently degrading to an
        // unisolated host process (false isolation the kubelet can't see). Only
        // host_network / no-CNI pods (no netns) — and non-linux — take the host path.
        #[cfg(target_os = "linux")]
        let mut ns_descriptor: Option<crate::ns_run::RunDescriptor> = {
            let pod_has_netns = self
                .cache()
                .sandboxes
                .get(&rec.sandbox.0)
                .and_then(|s| s.netns_path.clone())
                .is_some();
            if pod_has_netns {
                Some(self.build_ns_plan(&rec, &argv)?)
            } else {
                None
            }
        };
        #[cfg(not(target_os = "linux"))]
        let ns_descriptor: Option<crate::ns_run::RunDescriptor> = None;

        // WP-#102 (NS path only): create the exec-readiness pipe BEFORE building/
        // spawning `cmd`. The WRITE end (`wr`) travels — inherited (NOT O_CLOEXEC)
        // across the shim re-exec and threaded by the ns engine down to the
        // container's PID 1, which sets it CLOEXEC right before `execv` (success ⇒
        // EOF here) or writes error bytes on `execv` failure. The READ end (`rd`) is
        // set FD_CLOEXEC so the child process tree never inherits it. We block on
        // `rd` (with a timeout) AFTER spawn and persist `Running` only on EOF — so a
        // container is `Running` only once its workload has actually `execv`'d (audit
        // finding D; KPI-3 cold-start is now execv-milestone-aligned). The host path
        // is untouched (no pipe, persists Running pre-spawn as before).
        #[cfg(target_os = "linux")]
        let mut readiness_rd: Option<std::os::unix::io::RawFd> = None;
        #[cfg(target_os = "linux")]
        if let Some(desc) = ns_descriptor.as_mut() {
            let mut fds = [0 as libc::c_int; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(BackendError::Internal(format!(
                    "exec-readiness pipe for container {}: {}",
                    id.0,
                    std::io::Error::last_os_error()
                )));
            }
            let (rd, wr) = (fds[0], fds[1]);
            // rd CLOEXEC: the shim child tree must NOT hold the read end (only the
            // backend reads it). wr is deliberately LEFT non-CLOEXEC so it survives
            // the shim's re-exec down to PID 1 (pipe() fds are non-CLOEXEC by default).
            unsafe { libc::fcntl(rd, libc::F_SETFD, libc::FD_CLOEXEC) };
            desc.exec_ready_fd = Some(wr);
            readiness_rd = Some(rd);
        }

        let mut cmd = if let Some(desc) = &ns_descriptor {
            // ── NS path: re-exec `<current_exe> __ns-run`; descriptor in env. ──
            // current_exe is required to re-exec; if it somehow fails we cannot take
            // the ns path — but `try_build_ns_plan` already hydrated, so prefer an
            // honest spawn error over a silent rootfs-less host run.
            let exe = std::env::current_exe()
                .map_err(|e| BackendError::Internal(format!("current_exe for __ns-run: {e}")))?;
            let mut c = std::process::Command::new(exe);
            c.arg("__ns-run");
            // The RunDescriptor travels in an ENV var (mirrors `__ns-exec`'s
            // LIGHTR_NSEXEC_DESC), so the shim's STDIN stays FREE for the workload.
            // The shim `remove_var`s it before `engine.run`, so it never reaches the
            // container. A serialize failure here is fatal — fail closed rather than
            // spawn a shim that will exit with "env unset".
            let desc_json = serde_json::to_string(desc)
                .map_err(|e| BackendError::Internal(format!("serialize ns descriptor: {e}")))?;
            c.env(crate::ns_run::NSRUN_DESC_ENV, desc_json);
            // stdout/stderr are the container's, teed to the CRI log. stdin is the
            // container's own stdin now (the descriptor moved to env): a backend-held
            // pipe when the container requested it (so `open_attach` can WRITE to the
            // live workload — the critest attach test's interactive `/bin/sh`), else
            // /dev/null so a non-interactive workload gets a benign EOF (byte-identical
            // outcome to slice 1's post-write stdin close, but WITHOUT the descriptor
            // bytes). `register_io_and_tee` adopts the piped stdin into the io-table.
            c.stdout(std::process::Stdio::piped());
            c.stderr(std::process::Stdio::piped());
            c.stdin(if rec.config.stdin {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            });
            c
        } else {
            // ── HOST path: today's exact behavior (unchanged). ──
            let mut c = std::process::Command::new(&program);
            c.args(&argv[1..]);
            if !rec.config.working_dir.is_empty() {
                c.current_dir(&rec.config.working_dir);
            }
            for (k, v) in &rec.config.envs {
                c.env(k, v);
            }
            c.stdout(std::process::Stdio::piped());
            c.stderr(std::process::Stdio::piped());
            // stdin piped when requested (attach feeds the live process — WP-CRI-STREAM), else null.
            c.stdin(if rec.config.stdin {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            });
            // §D: on linux, join the MAIN process into the sandbox netns (sandbox.rs).
            #[cfg(target_os = "linux")]
            self.join_container_netns(&mut c, &rec.sandbox)?;
            c
        };

        // Persist start-intent BEFORE spawning (crash-only). WP-#102: the NS path
        // persists an HONEST `Created` (NOT Running) intent — `Running` is written
        // only AFTER the workload `execv`'s (the readiness wait below). A crash
        // mid-ns-start now leaves `Created`, never a false `Running` (strictly better
        // than the pre-#102 `lost-start-window` recovery, which downgraded a false
        // `Running` to Exited/-1). The HOST path keeps persisting `Running` pre-spawn
        // exactly as before (its workload is the spawned process — no exec milestone
        // to await).
        let started_at = now_nanos();
        {
            let mut cache = self.cache();
            let entry = cache
                .containers
                .get_mut(&id.0)
                .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;
            entry.state = if ns_descriptor.is_some() {
                ContainerState::Created
            } else {
                ContainerState::Running
            };
            entry.started_at_nanos = started_at;
            entry.pid = 0;
            entry.reason = "starting".to_string();
            let snap = entry.clone();
            drop(cache);
            self.persist(&snap)?;
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let mut cache = self.cache();
                if let Some(entry) = cache.containers.get_mut(&id.0) {
                    entry.state = ContainerState::Exited;
                    entry.finished_at_nanos = now_nanos();
                    entry.exit_code = -1;
                    entry.reason = "spawn-failed".to_string();
                    entry.message = e.to_string();
                    let snap = entry.clone();
                    drop(cache);
                    let _ = self.persist(&snap);
                }
                return Err(BackendError::Internal(format!(
                    "spawn container {}: {e}",
                    id.0
                )));
            }
        };

        let child_pid = child.id();

        // WP-#102 (NS path): the shim has been forked with the pipe WRITE end
        // inherited; CLOSE the backend's own copy NOW, immediately after spawn. If
        // the backend kept it open, the read end would never see EOF (the backend
        // itself would be a lingering writer) — so we would block until the container
        // exits instead of until its workload `execv`'s (THE #1 risk). The descriptor
        // still carries the fd NUMBER (in the LIGHTR_NSRUN_DESC env set on `cmd`) —
        // the shim's INHERITED copy is what reaches PID 1, unaffected by this close.
        #[cfg(target_os = "linux")]
        if let Some(desc) = &ns_descriptor {
            if let Some(wr) = desc.exec_ready_fd {
                unsafe { libc::close(wr) };
            }
        }

        // WP-#99 → attach fix: the `RunDescriptor` now travels in the
        // `LIGHTR_NSRUN_DESC` env var (set on `cmd` above), NOT on stdin — so the
        // shim's STDIN is the container's own stdin. We must NOT take/write it here:
        // `register_io_and_tee` (below) adopts the piped stdin write-end into the
        // io-table so `open_attach` can feed the live workload, exactly like the host
        // path. For a non-stdin container the shim's stdin is /dev/null (benign EOF).

        // Tee stdout/stderr to the CRI log and (on unix) register the live stdio
        // in the io-table for `open_attach` (WP-CRI-STREAM) — the SAME single
        // reader fans raw bytes to attachers, no second reader racing the log.
        self.register_io_and_tee(id, &mut child, &log_shared);

        // WP-#102 READINESS WAIT (NS path only): block on the read end until the
        // container's PID 1 `execv`'s. `register_io_and_tee` ran FIRST so the engine's
        // execv-failure eprintln (and any container output) lands in the CRI log. EOF
        // ⇒ exec SUCCEEDED ⇒ fall through to persist `Running`. Bytes/timeout ⇒ the
        // helper has already persisted `Exited`, reaped the shim, and (on timeout)
        // killed the cgroup; it returns `Err` and we fail the start (fail-closed —
        // never a false `Running`). The HOST path has no pipe and skips this entirely.
        #[cfg(target_os = "linux")]
        if let Some(rd) = readiness_rd {
            let cgroup_name = ns_descriptor
                .as_ref()
                .map(|d| d.cgroup_name.clone())
                .unwrap_or_default();
            self.wait_exec_ready(id, &mut child, rd, &cgroup_name)?;
        }

        // Persist the real pid (crash-only) + flip to `Running`. WP-#102: for the NS
        // path this is the FIRST `Running` write (the pre-spawn intent was `Created`),
        // reached only after the readiness wait above confirmed the workload `execv`'d.
        // For the HOST path the state is already `Running` (pre-spawn) — setting it
        // again is idempotent. For the NS path also persist the engine marker + the
        // cgroup leaf so `stop` knows to `cgroup.kill` (the shim pid alone is NOT the
        // in-pidns PID 1; killing it would orphan the container).
        {
            let mut cache = self.cache();
            let entry = cache
                .containers
                .get_mut(&id.0)
                .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;
            entry.state = ContainerState::Running;
            entry.pid = child_pid;
            entry.reason = String::new();
            if let Some(desc) = &ns_descriptor {
                entry.engine = "ns".to_string();
                entry.cgroup_name = desc.cgroup_name.clone();
            }
            let snap = entry.clone();
            drop(cache);
            self.persist(&snap)?;
        }

        // Detached reaper: SINGLE source of truth for the terminal exit code.
        let containers_dir = self.containers_dir();
        let cid = id.clone();
        let cache_arc = Arc::clone(&self.cache);
        #[cfg(unix)]
        let io_table_arc = Arc::clone(&self.io_table);
        std::thread::spawn(move || {
            let status = child.wait();
            let finished_at = now_nanos();
            let (exit_code, reason) = match status {
                Ok(s) => signal_or_code(&s),
                Err(e) => (-1, format!("wait-error: {e}")),
            };
            // Drop the held stdio on exit (no fds linger past the process).
            #[cfg(unix)]
            io_table_arc.lock().unwrap().remove(&cid.0);
            let mut cache = cache_arc.lock().unwrap();
            if let Some(entry) = cache.containers.get_mut(&cid.0) {
                if entry.state == ContainerState::Running {
                    entry.state = ContainerState::Exited;
                    entry.exit_code = exit_code;
                    entry.finished_at_nanos = finished_at;
                    entry.reason = reason;
                    let fname = format!("{}.json", cid.0);
                    let _ = atomic_write_json(&containers_dir, &fname, entry);
                }
            }
        });

        Ok(())
    }
}
