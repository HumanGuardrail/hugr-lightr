//! Container plane — create / start / stop / remove / status / list (WP-CRI-MVP).
//!
//! PROVENANCE: the lifecycle semantics (Created→Running→Exited, the start-intent
//! persist, the detached reaper that owns the terminal exit code, the
//! SIGTERM→SIGKILL grace in `stop`, the force-stop-then-remove in `remove`, the
//! CRI-log tee in `<RFC3339Nano> <stream> <F|P>` framing) are TRANSCRIBED from
//! the conformance reference `lightr-cri-fake`. Execution is a REAL host process
//! (no isolation yet — sandbox/netns is WP-CRI-SANDBOX).
//!
//! REUSE NOTE (ambiguity resolved, transcribe-don't-design): the brief points
//! at `lightr_run::spawn_detached_engine` for `start_container`. That engine
//! path derives its run-dir root from the PROCESS-GLOBAL `LIGHTR_HOME` env (see
//! lightr-run `run/paths.rs::lightr_home`) and writes null stdio with no
//! CRI-format log tee. Using it would (a) break per-instance state injection
//! (the backend roots state at the injected `home`, not at an env var) and so
//! break parallel-safe tests, and (b) drop the kubelet log framing critest
//! asserts. The fake — the conformance reference the brief says to TRANSCRIBE —
//! spawns the command directly and tees to the CRI log. We mirror the fake: a
//! real `std::process::Command` + a reaper thread + the tee. The supervisor
//! wiring (`spawn_detached_engine`) is the right call once a sandbox/netns root
//! is injectable; that is WP-CRI-SANDBOX. State still persists crash-only under
//! `<home>/cri/containers/`.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};

use crate::util::{
    atomic_write_json, now_nanos, open_cri_log, pid_alive, rec_to_status, signal_or_code,
    spawn_tee_thread, ContainerRecord,
};
use crate::vocab::{
    BackendError, ContainerConfig, ContainerFilter, ContainerId, ContainerState, ContainerStatus,
    Result, SandboxId,
};
use crate::LightrBackend;

/// In-memory cache (a view rebuilt from disk on open). Containers are keyed by
/// their id string. Sandbox state lives in WP-CRI-SANDBOX; the MVP container
/// plane needs only its own records plus the sandbox's `log_directory`, which
/// it reads from the persisted sandbox record when present (None today).
#[derive(Default)]
pub struct Cache {
    pub containers: BTreeMap<String, ContainerRecord>,
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

    fn cache(&self) -> std::sync::MutexGuard<'_, Cache> {
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

        // sandbox log_directory (None today — WP-CRI-SANDBOX persists sandbox
        // records; until then the relative log_path is used as-is).
        let sandbox_log_dir = self.sandbox_log_dir(&rec.sandbox);

        // Open (create) the CRI log so the empty file exists from start (§C).
        let log = open_cri_log(&sandbox_log_dir, &rec.config.log_path).map_err(BackendError::Io)?;
        let log_shared: Arc<Mutex<Option<fs::File>>> = Arc::new(Mutex::new(log));

        // Build the argv. Empty command/args ⇒ keep-alive `tail -f /dev/null`
        // (transcribed from the fake: critest's synthetic images carry no
        // entrypoint, and the container must stay Running for exec). A real
        // image-config entrypoint is the job of the rootfs path (WP-CRI-SANDBOX).
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
        cmd.stdin(std::process::Stdio::null());

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

        // Tee stdout/stderr to the CRI log (one thread per stream).
        if let Some(out) = child.stdout.take() {
            spawn_tee_thread("stdout", out, Arc::clone(&log_shared));
        }
        if let Some(err) = child.stderr.take() {
            spawn_tee_thread("stderr", err, Arc::clone(&log_shared));
        }

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
        std::thread::spawn(move || {
            let status = child.wait();
            let finished_at = now_nanos();
            let (exit_code, reason) = match status {
                Ok(s) => signal_or_code(&s),
                Err(e) => (-1, format!("wait-error: {e}")),
            };
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

    // ── status / list ────────────────────────────────────────────────────────

    pub(crate) fn container_status_impl(&self, id: &ContainerId) -> Result<ContainerStatus> {
        let cache = self.cache();
        let rec = cache
            .containers
            .get(&id.0)
            .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;
        let log_dir = self.sandbox_log_dir(&rec.sandbox);
        Ok(rec_to_status(rec, &log_dir))
    }

    pub(crate) fn list_containers_impl(
        &self,
        filter: &ContainerFilter,
    ) -> Result<Vec<ContainerStatus>> {
        let cache = self.cache();
        let mut out = Vec::new();
        for r in cache.containers.values() {
            if crate::util::container_matches(r, filter) {
                let log_dir = self.sandbox_log_dir(&r.sandbox);
                out.push(rec_to_status(r, &log_dir));
            }
        }
        Ok(out)
    }
}
