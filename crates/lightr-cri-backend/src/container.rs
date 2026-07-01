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

#[cfg(target_os = "linux")]
use crate::container_wait::pid_is_container_init;
use crate::util::{atomic_write_json, now_nanos, pid_alive, ContainerRecord};
#[cfg(target_os = "linux")]
use crate::vocab::BackendError;
use crate::vocab::{ContainerConfig, ContainerId, ContainerState, Result, SandboxId};
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
    ///
    /// WP-#102: the NS path no longer persists `Running` pre-spawn — it persists
    /// `Created` and flips to `Running` only AFTER the workload `execv`'s. So a crash
    /// mid-ns-start now leaves `Created` (no false `Running` to reconcile — strictly
    /// better). The pid-0 `lost-start-window` branch below now applies only to the
    /// HOST path (which still persists `Running` pre-spawn) and to legacy records.
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

    pub(crate) fn persist(&self, rec: &ContainerRecord) -> Result<()> {
        let fname = format!("{}.json", rec.id.0);
        atomic_write_json(&self.containers_dir(), &fname, rec)
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
            engine: String::new(),
            cgroup_name: String::new(),
        };
        // Crash-only: persist BEFORE inserting into the cache and returning.
        self.persist(&rec)?;
        self.cache().containers.insert(id.0.clone(), rec);
        Ok(id)
    }

    // ── WP-#99: cgroup-based stop for the NS path (linux only) ────────────────

    /// Stop an `ns`-path container by acting on its cgroup-v2 leaf
    /// (`/sys/fs/cgroup/<cgroup_name>`). `grace > 0` first SIGTERMs every process
    /// in the cgroup (a chance for a clean shutdown — the workload PID 1 only acts
    /// on it if it installed a handler, exactly like Docker), waits up to the grace
    /// period for the reaper to record the exit, then unconditionally writes
    /// `cgroup.kill` (atomic SIGKILL of the whole subtree — idempotent and a no-op
    /// on an already-empty cgroup, so it guarantees nothing lingers). `grace == 0`
    /// goes straight to `cgroup.kill`. The detached reaper records the real exit
    /// code; we only deliver the kill + wait for it to land (synchronous `stop`).
    #[cfg(target_os = "linux")]
    fn cgroup_stop(&self, rec: &ContainerRecord, id: &ContainerId, grace_seconds: i64) {
        let leaf = std::path::Path::new("/sys/fs/cgroup").join(&rec.cgroup_name);
        let kill_file = leaf.join("cgroup.kill");

        if grace_seconds > 0 {
            // SIGTERM every process currently in the cgroup.
            if let Ok(procs) = fs::read_to_string(leaf.join("cgroup.procs")) {
                for line in procs.lines() {
                    if let Ok(pid) = line.trim().parse::<i32>() {
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(pid as libc::pid_t, libc::SIGTERM);
                        }
                    }
                }
            }
            let grace = std::time::Duration::from_secs(grace_seconds as u64);
            self.wait_until_exited(id, grace);
            // Always finish with cgroup.kill: guarantees the in-pidns PID 1 + all
            // descendants are gone even if SIGTERM was ignored (idempotent).
            Self::cgroup_force_kill(&leaf, &kill_file);
            self.wait_until_exited(id, std::time::Duration::from_secs(5));
        } else {
            Self::cgroup_force_kill(&leaf, &kill_file);
            self.wait_until_exited(id, std::time::Duration::from_secs(5));
        }
    }

    /// Kill every process in the container cgroup. AUDIT FIX (#99): `cgroup.kill`
    /// (cgroup v2, kernel ≥5.14) is the atomic primitive, but it does NOT exist on
    /// older kernels — the previous `let _ = fs::write(cgroup.kill, "1")` SWALLOWED
    /// that error, so `stop` silently no-op'd and the container leaked while
    /// returning `Ok`. Now: try `cgroup.kill`; if the write fails (missing file /
    /// error), FALL BACK to SIGKILL'ing every pid in `cgroup.procs` so the
    /// container is actually torn down rather than silently surviving.
    #[cfg(unix)]
    pub(crate) fn cgroup_force_kill(leaf: &std::path::Path, kill_file: &std::path::Path) {
        if fs::write(kill_file, b"1").is_ok() {
            return;
        }
        // cgroup.kill unavailable/failed → SIGKILL the cgroup's members directly.
        match fs::read_to_string(leaf.join("cgroup.procs")) {
            Ok(procs) => {
                eprintln!(
                    "lightr-cri: cgroup.kill unavailable at {} — falling back to SIGKILL of cgroup.procs",
                    kill_file.display()
                );
                for line in procs.lines() {
                    if let Ok(pid) = line.trim().parse::<i32>() {
                        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                    }
                }
            }
            Err(e) => eprintln!(
                "lightr-cri: stop could not cgroup.kill NOR read cgroup.procs at {}: {e} — container may leak",
                leaf.display()
            ),
        }
    }
    #[cfg(not(unix))]
    #[allow(dead_code)] // only called from cgroup_stop (cfg linux); a genuine stub on non-unix
    pub(crate) fn cgroup_force_kill(_leaf: &std::path::Path, _kill_file: &std::path::Path) {}

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

        // WP-#99 (NS path): kill via the cgroup, not the shim pid. The recorded
        // `rec.pid` is the `__ns-run` SHIM — an ancestor of the container's PID 1
        // (which lives in a child pid namespace). `kill(shim)` would NOT take down
        // the in-pidns PID 1 + its descendants; `cgroup.kill` atomically SIGKILLs
        // the WHOLE subtree (the setup process + PID 1 + every descendant). The
        // reaper still records the terminal exit. Linux-only.
        #[cfg(target_os = "linux")]
        if rec.engine == "ns" && !rec.cgroup_name.is_empty() {
            self.cgroup_stop(&rec, id, grace_seconds);
            return Ok(());
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
        // WP-#99: also drop the per-container dir (the hydrated rootfs lives at
        // `<containers>/<cid>/rootfs`). Best-effort — a leftover dir must not fail
        // an otherwise-idempotent remove; the record sidecar above is the gate.
        let dir = self.containers_dir().join(&id.0);
        let _ = fs::remove_dir_all(dir);
        Ok(())
    }
}

// ── WP-#100 (CRI exec slice 1): resolve the container's in-pidns PID 1 ────────
//
// The recorded `rec.pid` is the `__ns-run` SHIM, which lives in the HOST
// namespaces and is NOT in the container cgroup — so it is the wrong target for
// `setns`. The container's real PID 1 (in the user+mnt+pid+net namespaces) is a
// DIFFERENT host pid the engine hides. We recover it WITHOUT extending the engine
// seam: read the container cgroup's `cgroup.procs` (which holds only the setup
// process + PID 1 + descendants — never the shim) and pick the member whose
// INNERMOST NSpid field is `1` (it is PID 1 in its own pid namespace).
#[cfg(target_os = "linux")]
impl LightrBackend {
    /// Resolve the host pid of the container's in-pidns PID 1 from its cgroup-v2
    /// leaf. Reads `/sys/fs/cgroup/<cgroup_name>/cgroup.procs` and returns the
    /// member whose `/proc/<pid>/status` `NSpid:` line ends in `1` (PID 1 of the
    /// container's own pid namespace). The setup process has a single NSpid field
    /// (host pidns only); workload descendants end in `>1`. Retried briefly: right
    /// after start, `cgroup.procs` can momentarily hold only the setup process
    /// before PID 1 forks. Fail-closed (retryable `FailedPrecondition`) if no
    /// innermost-NSpid==1 member appears — NEVER falls back to a host exec (that
    /// would run OUTSIDE the container = a false result).
    pub(crate) fn container_pid1(&self, cgroup_name: &str) -> Result<u32> {
        if cgroup_name.is_empty() {
            return Err(BackendError::FailedPrecondition(
                "container_pid1: empty cgroup_name (not an ns container?)".to_string(),
            ));
        }
        let procs_path = std::path::Path::new("/sys/fs/cgroup")
            .join(cgroup_name)
            .join("cgroup.procs");

        for _ in 0..20 {
            if let Ok(procs) = fs::read_to_string(&procs_path) {
                for line in procs.lines() {
                    let pid: u32 = match line.trim().parse() {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    if pid_is_container_init(pid) {
                        return Ok(pid);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(BackendError::FailedPrecondition(format!(
            "container_pid1: no PID-1 (innermost NSpid==1) in {} after retries",
            procs_path.display()
        )))
    }
}
