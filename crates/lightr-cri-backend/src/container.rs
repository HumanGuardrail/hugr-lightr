//! Container plane — create / start / stop / remove / status / list (WP-CRI-MVP).
//!
//! PROVENANCE: the lifecycle semantics (Created→Running→Exited, the start-intent
//! persist, the detached reaper that owns the terminal exit code, the
//! SIGTERM→SIGKILL grace in `stop`, the force-stop-then-remove in `remove`, the
//! CRI-log tee in `<RFC3339Nano> <stream> <F|P>` framing) are TRANSCRIBED from
//! the conformance reference `lightr-cri-fake`. Execution is a REAL host process;
//! on linux it joins the sandbox netns at spawn (WP-CRI-SANDBOX wired the gate +
//! netns-join; the helpers live in sandbox.rs).
//!
//! REUSE NOTE (transcribe-don't-design): the brief points at
//! `lightr_run::spawn_detached_engine`, but that path roots its run-dir at the
//! PROCESS-GLOBAL `LIGHTR_HOME` env and writes null stdio with no CRI log tee —
//! breaking per-instance state injection (so parallel tests) and the kubelet log
//! framing critest asserts. So we mirror the fake instead: a real
//! `std::process::Command` + a reaper thread + the tee, persisting crash-only
//! under `<home>/cri/containers/`.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};

use crate::util::{
    atomic_write_json, now_nanos, open_cri_log, pid_alive, signal_or_code, ContainerRecord,
};
use crate::vocab::{BackendError, ContainerConfig, ContainerId, ContainerState, Result, SandboxId};
use crate::LightrBackend;

/// In-memory cache (a view rebuilt from disk on open; crash-only law). Both
/// halves keyed by id string; the sandbox half is owned by `sandbox.rs`.
#[derive(Default)]
pub struct Cache {
    pub containers: BTreeMap<String, ContainerRecord>,
    pub sandboxes: BTreeMap<String, crate::sandbox::SandboxRecord>,
}

impl LightrBackend {
    // ── open / recovery ──────────────────────────────────────────────────────

    /// Rebuild the container cache from disk, reconciling Running records whose
    /// backing process is gone (crash-recovery law, transcribed from the fake):
    /// a Running record with a dead pid recovers as Exited/-1
    /// `lost-exit-reaped-elsewhere`; a Running record with pid 0 (crash between
    /// spawn and pid-persist) recovers as Exited/-1 `lost-start-window`.
    pub(crate) fn load_container_cache(&self) -> Cache {
        let dir = self.containers_dir();
        let mut cache = Cache::default();
        if let Ok(rd) = fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(data) = fs::read(&path) else { continue };
                let Ok(mut rec) = serde_json::from_slice::<ContainerRecord>(&data) else {
                    continue;
                };
                if rec.state == ContainerState::Running {
                    if rec.pid > 0 && !pid_alive(rec.pid) {
                        rec.state = ContainerState::Exited;
                        rec.exit_code = -1;
                        rec.reason = "lost-exit-reaped-elsewhere".to_string();
                        rec.finished_at_nanos = now_nanos();
                        let fname = format!("{}.json", rec.id.0);
                        let _ = atomic_write_json(&dir, &fname, &rec);
                    } else if rec.pid == 0 {
                        rec.state = ContainerState::Exited;
                        rec.exit_code = -1;
                        rec.reason = "lost-start-window".to_string();
                        rec.finished_at_nanos = now_nanos();
                        let fname = format!("{}.json", rec.id.0);
                        let _ = atomic_write_json(&dir, &fname, &rec);
                    }
                }
                cache.containers.insert(rec.id.0.clone(), rec);
            }
        }
        cache
    }

    pub(crate) fn cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap()
    }

    fn persist(&self, rec: &ContainerRecord) -> Result<()> {
        let fname = format!("{}.json", rec.id.0);
        atomic_write_json(&self.containers_dir(), &fname, rec)
    }

    /// Poll the cache until the container is no longer Running (its reaper has
    /// recorded the terminal state), or `timeout` elapses. The reaper owns the
    /// real exit code; `stop` only waits for it to land so the call is
    /// synchronous to the caller. Transcribed from the fake.
    fn wait_until_exited(&self, id: &ContainerId, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            {
                let cache = self.cache();
                match cache.containers.get(&id.0) {
                    Some(r) if r.state != ContainerState::Running => return true,
                    None => return true,
                    _ => {}
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // ── create ───────────────────────────────────────────────────────────────

    pub(crate) fn create_container_impl(
        &self,
        sandbox: &SandboxId,
        cfg: ContainerConfig,
    ) -> Result<ContainerId> {
        // Sandbox gate (state law): exist+Ready else NotFound/FailedPrecondition.
        self.ensure_sandbox_ready(sandbox)?;
        let id = ContainerId(crate::util::new_id("ct-"));
        let rec = ContainerRecord {
            id: id.clone(),
            sandbox: sandbox.clone(),
            config: cfg,
            state: ContainerState::Created,
            created_at_nanos: now_nanos(),
            started_at_nanos: 0,
            finished_at_nanos: 0,
            exit_code: 0,
            reason: String::new(),
            message: String::new(),
            pid: 0,
        };
        // Crash-only: persist BEFORE inserting into the cache and returning.
        self.persist(&rec)?;
        self.cache().containers.insert(id.0.clone(), rec);
        Ok(id)
    }

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

        let mut cmd = std::process::Command::new(&program);
        cmd.args(&argv[1..]);
        if !rec.config.working_dir.is_empty() {
            cmd.current_dir(&rec.config.working_dir);
        }
        for (k, v) in &rec.config.envs {
            cmd.env(k, v);
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // stdin piped when requested (attach feeds the live process — WP-CRI-STREAM), else null.
        cmd.stdin(if rec.config.stdin {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });

        // §D: on linux, join the MAIN process into the sandbox netns (sandbox.rs).
        #[cfg(target_os = "linux")]
        self.join_container_netns(&mut cmd, &rec.sandbox)?;

        // Persist start-intent (Running, pid 0) BEFORE spawning (crash-only).
        let started_at = now_nanos();
        {
            let mut cache = self.cache();
            let entry = cache
                .containers
                .get_mut(&id.0)
                .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;
            entry.state = ContainerState::Running;
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

        // Tee stdout/stderr to the CRI log and (on unix) register the live stdio
        // in the io-table for `open_attach` (WP-CRI-STREAM) — the SAME single
        // reader fans raw bytes to attachers, no second reader racing the log.
        self.register_io_and_tee(id, &mut child, &log_shared);

        // Persist the real pid (crash-only).
        {
            let mut cache = self.cache();
            let entry = cache
                .containers
                .get_mut(&id.0)
                .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;
            entry.pid = child_pid;
            entry.reason = String::new();
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

    // ── stop (SIGTERM→SIGKILL grace) ─────────────────────────────────────────

    pub(crate) fn stop_container_impl(&self, id: &ContainerId, grace_seconds: i64) -> Result<()> {
        let rec = match self.cache().containers.get(&id.0) {
            Some(r) => r.clone(),
            None => return Ok(()), // idempotent
        };
        match rec.state {
            ContainerState::Created | ContainerState::Exited | ContainerState::Unknown => {
                return Ok(())
            }
            ContainerState::Running => {}
        }

        // grace > 0 → SIGTERM, wait up to grace, then SIGKILL. grace == 0 →
        // immediate SIGKILL. The reaper records the real code (143 / 137); we
        // only deliver signals + wait. Unix-only (the windows gate compiles
        // this crate but does not run it; deliver no signal there).
        if rec.pid > 0 {
            #[cfg(unix)]
            {
                if grace_seconds > 0 {
                    unsafe { libc::kill(rec.pid as libc::pid_t, libc::SIGTERM) };
                    let grace = std::time::Duration::from_secs(grace_seconds as u64);
                    if !self.wait_until_exited(id, grace) {
                        unsafe { libc::kill(rec.pid as libc::pid_t, libc::SIGKILL) };
                        self.wait_until_exited(id, std::time::Duration::from_secs(5));
                    }
                } else {
                    unsafe { libc::kill(rec.pid as libc::pid_t, libc::SIGKILL) };
                    self.wait_until_exited(id, std::time::Duration::from_secs(5));
                }
            }
            #[cfg(not(unix))]
            {
                let _ = grace_seconds;
                self.wait_until_exited(id, std::time::Duration::from_secs(5));
            }
            return Ok(());
        }

        // Defensive: a Running record with no backing process has no reaper —
        // record the terminal state directly (transcribed from the fake).
        let mut cache = self.cache();
        if let Some(entry) = cache.containers.get_mut(&id.0) {
            if entry.state == ContainerState::Running {
                entry.state = ContainerState::Exited;
                entry.finished_at_nanos = now_nanos();
                entry.exit_code = if grace_seconds > 0 { 128 + 15 } else { 128 + 9 };
                entry.reason = "stopped".to_string();
                let snap = entry.clone();
                drop(cache);
                self.persist(&snap)?;
            }
        }
        Ok(())
    }

    // ── remove (force-stop if Running, then cleanup) ─────────────────────────

    pub(crate) fn remove_container_impl(&self, id: &ContainerId) -> Result<()> {
        let is_running = match self.cache().containers.get(&id.0) {
            None => return Ok(()), // idempotent
            Some(r) => r.state == ContainerState::Running,
        };
        if is_running {
            self.stop_container_impl(id, 0)?; // forced SIGKILL + reap
        }
        self.cache().containers.remove(&id.0);
        let path = self.containers_dir().join(format!("{}.json", id.0));
        let _ = fs::remove_file(path);
        Ok(())
    }
}
