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
            engine: String::new(),
            cgroup_name: String::new(),
        };
        // Crash-only: persist BEFORE inserting into the cache and returning.
        self.persist(&rec)?;
        self.cache().containers.insert(id.0.clone(), rec);
        Ok(id)
    }

    // ── WP-#99: NS-path planning + rootfs hydrate (linux only) ────────────────

    /// Build the `ns`-engine `RunDescriptor` (real image rootfs + pod netns) for an
    /// **isolation-expecting** pod — the caller has already confirmed the sandbox
    /// has a pinned netns. Returns `Err` (FAILING the container start) when the ns
    /// engine is unavailable or the image cannot hydrate.
    ///
    /// AUDIT FIX (#99): the previous `Option` contract silently fell back to an
    /// unisolated HOST process when hydrate/engine failed — for a pod that has an
    /// isolated netns, that is FALSE ISOLATION the kubelet cannot detect (the
    /// container is still reported `Running`). Fail-closed instead. host_network /
    /// no-CNI pods (no pinned netns) legitimately use the host path; the caller
    /// gates on that and never calls this.
    #[cfg(target_os = "linux")]
    fn build_ns_plan(
        &self,
        rec: &ContainerRecord,
        argv: &[String],
    ) -> Result<crate::ns_run::RunDescriptor> {
        let netns_path = self
            .cache()
            .sandboxes
            .get(&rec.sandbox.0)
            .and_then(|s| s.netns_path.clone())
            .ok_or_else(|| {
                BackendError::Internal("build_ns_plan called without a pod netns".to_string())
            })?;

        // The ns engine must be available (root + Linux). For an isolation-expecting
        // pod this is REQUIRED — an unavailable engine is a hard error, not a silent
        // host downgrade.
        lightr_engine::engine_for(lightr_engine::EngineKind::Ns).map_err(|e| {
            BackendError::Internal(format!(
                "ns engine unavailable for an isolation-expecting pod (container {}): {e}",
                rec.id.0
            ))
        })?;

        // Materialize the image rootfs from the CAS; a miss is a hard error (cannot
        // run the real container ⇒ refuse rather than run an unisolated host process).
        let rootfs = self.hydrate_rootfs(&rec.id, &rec.config.image_ref).map_err(|e| {
            BackendError::Internal(format!(
                "hydrate rootfs for container {} (image {:?}) failed: {e}",
                rec.id.0, rec.config.image_ref
            ))
        })?;

        // Capabilities from the v1.2 security context, when present (CRI style).
        let (cap_add, cap_drop) = match rec
            .config
            .security
            .as_ref()
            .and_then(|s| s.capabilities.as_ref())
        {
            Some(c) => (c.add.clone(), c.drop.clone()),
            None => (Vec::new(), Vec::new()),
        };

        Ok(crate::ns_run::RunDescriptor {
            rootfs,
            argv: argv.to_vec(),
            cwd: rec.config.working_dir.clone(),
            env: rec.config.envs.clone(),
            netns_path: Some(netns_path),
            // Deterministic, flat leaf so `stop` can rebuild the path and
            // `cgroup.kill` it (the record also persists this name).
            cgroup_name: format!("lightr-cri-{}", rec.id.0),
            // The frozen seam carries no read-only / shm-size / init for a
            // container; defaults (the ns engine still gives a default 64 MiB
            // /dev/shm). read_only/shm/init become reachable when the seam grows them.
            read_only: false,
            shm_size: None,
            init: false,
            cap_add,
            cap_drop,
        })
    }

    /// Materialize the image rootfs for `cid` from the CAS store into a persistent
    /// per-container dir (`<home>/cri/containers/<cid>/rootfs`) via
    /// `lightr_index::hydrate`. The store name is the SAME `sanitize_ref` the image
    /// pull tagged the bytes under. Idempotent: a non-empty existing rootfs (a
    /// restart) is reused. Honest `Err` (mapped) when the ref is absent from the
    /// store or hydration fails — the caller treats that as a host-path fallback.
    #[cfg(target_os = "linux")]
    fn hydrate_rootfs(
        &self,
        cid: &ContainerId,
        image_ref: &str,
    ) -> Result<std::path::PathBuf> {
        let store = lightr_store::Store::open(self.home().join("store"))
            .map_err(crate::util::map_lightr_err)?;
        let store_name = crate::images::sanitize_ref(image_ref);
        let rootfs = self
            .containers_dir()
            .join(&cid.0)
            .join("rootfs");

        // Reuse an already-hydrated rootfs (restart of the same container).
        if rootfs.exists() {
            let nonempty = fs::read_dir(&rootfs)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false);
            if nonempty {
                return Ok(rootfs);
            }
        }
        fs::create_dir_all(&rootfs).map_err(BackendError::Io)?;
        lightr_index::hydrate(&rootfs, &store, &store_name).map_err(crate::util::map_lightr_err)?;
        Ok(rootfs)
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

        // WP-#99 (CRI slice 1): decide the execution path. The NS path runs the
        // REAL image rootfs under the `ns` engine, joined into the pod's netns,
        // by re-exec'ing THIS binary as `__ns-run` with a `RunDescriptor` piped on
        // stdin. It is taken ONLY when (linux + the pod has a pinned netns + the
        // ns engine is available + the image hydrates). EVERY other case falls
        // back to today's host-process path (behavior-preserving) — `ns_descriptor`
        // is `None` there, including on non-linux (so the macOS gate is untouched).
        // AUDIT FIX (#99): gate on whether the POD expects isolation (has a pinned
        // netns from CNI). If it does, the ns plan MUST succeed — a hydrate/engine
        // failure FAILS the start (`?`) rather than silently degrading to an
        // unisolated host process (false isolation the kubelet can't see). Only
        // host_network / no-CNI pods (no netns) — and non-linux — take the host path.
        #[cfg(target_os = "linux")]
        let ns_descriptor: Option<crate::ns_run::RunDescriptor> = {
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

        let mut cmd = if ns_descriptor.is_some() {
            // ── NS path: re-exec `<current_exe> __ns-run`; descriptor on stdin. ──
            // current_exe is required to re-exec; if it somehow fails we cannot take
            // the ns path — but `try_build_ns_plan` already hydrated, so prefer an
            // honest spawn error over a silent rootfs-less host run.
            let exe = std::env::current_exe().map_err(|e| {
                BackendError::Internal(format!("current_exe for __ns-run: {e}"))
            })?;
            let mut c = std::process::Command::new(exe);
            c.arg("__ns-run");
            // stdin carries the descriptor (we write it post-spawn, then close);
            // stdout/stderr are the container's, teed to the CRI log.
            c.stdin(std::process::Stdio::piped());
            c.stdout(std::process::Stdio::piped());
            c.stderr(std::process::Stdio::piped());
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

        // WP-#99 (NS path): hand the `RunDescriptor` to the `__ns-run` shim over
        // its stdin, then CLOSE stdin (drop) so the shim's `read_to_end` returns
        // and it proceeds to run the ns engine. Done BEFORE `register_io_and_tee`
        // so the tee never tries to adopt this stdin as an attach sink (it takes
        // `child.stdin`, now already gone — so ns containers have no attach-stdin
        // in slice 1, acceptable). A write failure here means the shim will EOF on
        // empty stdin and fail closed (exit 1), which the reaper records honestly.
        if let Some(desc) = &ns_descriptor {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                match serde_json::to_vec(desc) {
                    Ok(bytes) => {
                        let _ = stdin.write_all(&bytes);
                    }
                    Err(e) => eprintln!("lightr-cri: serialize ns descriptor: {e}"),
                }
                // `stdin` drops here → EOF for the shim.
            }
        }

        // Tee stdout/stderr to the CRI log and (on unix) register the live stdio
        // in the io-table for `open_attach` (WP-CRI-STREAM) — the SAME single
        // reader fans raw bytes to attachers, no second reader racing the log.
        self.register_io_and_tee(id, &mut child, &log_shared);

        // Persist the real pid (crash-only). For the NS path also persist the
        // engine marker + the cgroup leaf so `stop` knows to `cgroup.kill` (the
        // shim pid alone is NOT the in-pidns PID 1; killing it would orphan the
        // container).
        {
            let mut cache = self.cache();
            let entry = cache
                .containers
                .get_mut(&id.0)
                .ok_or_else(|| BackendError::NotFound(format!("container {}", id.0)))?;
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
    fn cgroup_force_kill(leaf: &std::path::Path, kill_file: &std::path::Path) {
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
    fn cgroup_force_kill(_leaf: &std::path::Path, _kill_file: &std::path::Path) {}

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

/// True iff host `pid`'s `/proc/<pid>/status` `NSpid:` line has innermost (last)
/// field == 1 — i.e. it is PID 1 inside its own pid namespace (the container init).
#[cfg(target_os = "linux")]
fn pid_is_container_init(pid: u32) -> bool {
    let status = match fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(s) => s,
        Err(_) => return false, // raced away — not it
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("NSpid:") {
            // Fields are tab/space separated host→innermost; the LAST is the pid
            // in the deepest pid namespace. Setup has a single field (host only).
            if let Some(innermost) = rest.split_whitespace().next_back() {
                return innermost == "1";
            }
        }
    }
    false
}
