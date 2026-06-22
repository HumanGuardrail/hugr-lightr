//! Sandbox / pod plane — run / stop / remove / status / list + network_ready
//! (WP-CRI-SANDBOX).
//!
//! PROVENANCE: the state machine (Ready → NotReady on stop → gone on remove,
//! all idempotent), the crash-only record (`<home>/cri/sandboxes/<id>.json`,
//! atomic tmp+fsync+rename, cache rebuilt from disk on construction), the
//! remove→stop+cascade-to-containers law, the filter predicates, and the
//! cfg(linux) netns-pin + CNI ADD/DEL lifecycle are TRANSCRIBED from the
//! conformance reference `lightr-cri/crates/lightr-cri-fake` (run_sandbox /
//! stop_sandbox / remove_sandbox / sandbox_status / list_sandboxes /
//! network_ready) and the netns/CNI executor shape of
//! `lightr-cri/crates/lightr-cri-net` (netns::{create,teardown,join_netns},
//! chain::{add,del}). Transcribed, NOT a crate dep (house seam pattern,
//! ADR-0017); drift caught by the shared conformance vectors.
//!
//! PLATFORM (contract §5 + brief): the STATE MACHINE is fully macOS-testable
//! and is the gate here. The netns/CNI RUNTIME is cfg(linux) and probe-truthful
//! elsewhere — on macOS/non-linux a sandbox has its record + state machine,
//! `ip = None`, `network_ready() = false`. The real netns/CNI path is validated
//! only on Linux CI / on-box (NOT gate-verifiable on macOS) — noted per §5.

use std::collections::BTreeMap;
use std::fs;

use crate::util::{atomic_write_json, new_id, now_nanos};
use crate::vocab::{
    BackendError, Result, SandboxConfig, SandboxFilter, SandboxId, SandboxState, SandboxStatus,
};
use crate::LightrBackend;

/// On-disk sandbox record. Mirrors `SandboxStatus` plus the backend-owned
/// network handles (`ip`, `netns_path`) populated by the cfg(linux) CNI path.
/// `serde(default)` on the v1.1 fields so older state files load unchanged.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SandboxRecord {
    pub id: SandboxId,
    pub config: SandboxConfig,
    pub state: SandboxState,
    pub created_at_nanos: i64,
    /// Host-routable pod IP assigned by CNI ADD. None when host_network or CNI
    /// unavailable (macOS / unprivileged) — probe-truthful.
    #[serde(default)]
    pub ip: Option<String>,
    /// Path of the pinned netns bind-mount. None when host_network / no CNI.
    #[serde(default)]
    pub netns_path: Option<String>,
}

impl crate::container::Cache {
    /// Sandbox `log_directory` from an ALREADY-HELD cache guard (the container
    /// status/list/start paths hold the lock; re-locking the Mutex would
    /// self-deadlock). Empty when the sandbox is unknown — probe-truthful.
    pub(crate) fn sandbox_log_dir(&self, sandbox: &SandboxId) -> String {
        self.sandboxes
            .get(&sandbox.0)
            .map(|s| s.config.log_directory.clone())
            .unwrap_or_default()
    }
}

fn sandbox_rec_to_status(rec: &SandboxRecord) -> SandboxStatus {
    SandboxStatus {
        id: rec.id.clone(),
        config: rec.config.clone(),
        state: rec.state,
        created_at_nanos: rec.created_at_nanos,
        ip: rec.ip.clone(),
        netns_path: rec.netns_path.clone(),
    }
}

fn sandbox_matches(rec: &SandboxRecord, filter: &SandboxFilter) -> bool {
    if let Some(id) = &filter.id {
        if &rec.id != id {
            return false;
        }
    }
    if let Some(state) = &filter.state {
        if &rec.state != state {
            return false;
        }
    }
    for (k, v) in &filter.label_selector {
        if rec.config.labels.get(k).map(String::as_str) != Some(v.as_str()) {
            return false;
        }
    }
    true
}

impl LightrBackend {
    // ── open / recovery ──────────────────────────────────────────────────────

    /// Rebuild the sandbox half of the cache from disk (crash-only law: disk is
    /// the source of truth; a restarted backend re-derives state from it).
    /// Sandbox state is durable — Ready/NotReady is exactly what the last
    /// transition persisted, so (unlike containers) there is nothing to
    /// reconcile against a live process.
    pub(crate) fn load_sandbox_cache(&self) -> BTreeMap<String, SandboxRecord> {
        let dir = self.sandboxes_dir();
        let mut out = BTreeMap::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(data) = fs::read(&path) else { continue };
                let Ok(rec) = serde_json::from_slice::<SandboxRecord>(&data) else {
                    continue;
                };
                out.insert(rec.id.0.clone(), rec);
            }
        }
        out
    }

    fn persist_sandbox(&self, rec: &SandboxRecord) -> Result<()> {
        let fname = format!("{}.json", rec.id.0);
        atomic_write_json(&self.sandboxes_dir(), &fname, rec)
    }

    // ── run ────────────────────────────────────────────────────────────────

    pub(crate) fn run_sandbox_impl(&self, cfg: SandboxConfig) -> Result<SandboxId> {
        let id = SandboxId(new_id("sb-"));

        // §D: not host_network + CNI available → create+pin netns + CNI ADD.
        // On macOS / unprivileged: cni_available() is None → host-network
        // fallback (probe-truthful: ip=None, netns_path=None).
        let (ip, netns_path) = if cfg.host_network {
            (None, None)
        } else {
            cni_setup(&id, &cfg)?
        };

        let rec = SandboxRecord {
            id: id.clone(),
            config: cfg,
            state: SandboxState::Ready,
            created_at_nanos: now_nanos(),
            ip,
            netns_path,
        };
        // Crash-only: persist BEFORE inserting into the cache and returning.
        self.persist_sandbox(&rec)?;
        self.cache().sandboxes.insert(id.0.clone(), rec);
        Ok(id)
    }

    // ── stop (Ready → NotReady, idempotent) ──────────────────────────────────

    pub(crate) fn stop_sandbox_impl(&self, id: &SandboxId) -> Result<()> {
        let snap = {
            let mut cache = self.cache();
            let rec = match cache.sandboxes.get_mut(&id.0) {
                Some(r) => r,
                None => return Ok(()), // idempotent: already gone
            };
            if rec.state == SandboxState::NotReady {
                return Ok(()); // idempotent: already stopped
            }
            rec.state = SandboxState::NotReady;
            rec.clone()
        };
        // Crash-only: persist the transition BEFORE returning.
        self.persist_sandbox(&snap)?;
        Ok(())
    }

    // ── remove (idempotent; implies stop; cascades to its containers) ─────────

    pub(crate) fn remove_sandbox_impl(&self, id: &SandboxId) -> Result<()> {
        // First stop it (idempotent).
        self.stop_sandbox_impl(id)?;

        // Collect this sandbox's containers + snapshot netns for teardown.
        let (container_ids, netns_path) = {
            let cache = self.cache();
            if !cache.sandboxes.contains_key(&id.0) {
                return Ok(()); // already gone
            }
            let containers: Vec<crate::vocab::ContainerId> = cache
                .containers
                .values()
                .filter(|c| &c.sandbox == id)
                .map(|c| c.id.clone())
                .collect();
            let ns = cache
                .sandboxes
                .get(&id.0)
                .and_then(|s| s.netns_path.clone());
            (containers, ns)
        };

        // Stop + remove each container (cascade).
        for cid in &container_ids {
            self.stop_container_impl(cid, 0)?;
            self.remove_container_impl(cid)?;
        }

        // §D: CNI DEL + netns teardown (cfg(linux); idempotent, fail-closed).
        if let Some(ref ns_path) = netns_path {
            cni_teardown(id, ns_path);
        }

        // Remove the record (cache + disk).
        {
            let mut cache = self.cache();
            if cache.sandboxes.remove(&id.0).is_none() {
                return Ok(());
            }
        }
        let path = self.sandboxes_dir().join(format!("{}.json", id.0));
        let _ = fs::remove_file(path);
        Ok(())
    }

    // ── status / list ────────────────────────────────────────────────────────

    pub(crate) fn sandbox_status_impl(&self, id: &SandboxId) -> Result<SandboxStatus> {
        let cache = self.cache();
        cache
            .sandboxes
            .get(&id.0)
            .map(sandbox_rec_to_status)
            .ok_or_else(|| BackendError::NotFound(format!("sandbox {}", id.0)))
    }

    pub(crate) fn list_sandboxes_impl(&self, filter: &SandboxFilter) -> Result<Vec<SandboxStatus>> {
        let cache = self.cache();
        Ok(cache
            .sandboxes
            .values()
            .filter(|r| sandbox_matches(r, filter))
            .map(sandbox_rec_to_status)
            .collect())
    }

    /// Sandbox gate for `create_container` (state law): the sandbox must exist
    /// (else NotFound) and be Ready (else FailedPrecondition). Transcribed from
    /// the fake. Owned here so container.rs only calls it.
    pub(crate) fn ensure_sandbox_ready(&self, sandbox: &SandboxId) -> Result<()> {
        let cache = self.cache();
        match cache.sandboxes.get(&sandbox.0) {
            None => Err(BackendError::NotFound(format!("sandbox {}", sandbox.0))),
            Some(sb) if sb.state != SandboxState::Ready => Err(BackendError::FailedPrecondition(
                format!("sandbox {} is not Ready", sandbox.0),
            )),
            Some(_) => Ok(()),
        }
    }

    /// Probe-truthful network readiness (contract §D). True iff CNI is wired
    /// and available; on macOS / unprivileged this is false (host-network
    /// behavior — no pod network claimed).
    pub(crate) fn network_ready_impl(&self) -> bool {
        cni_available()
    }

    /// cfg(linux) container netns-join: register a `pre_exec` setns(CLONE_NEWNET)
    /// on `cmd` joining the sandbox's pinned netns when it has a recorded
    /// `netns_path` (the CNI path set it). No-op when None (host_network /
    /// no-CNI). The netns fd is opened in the PARENT (pre-fork) and moved into
    /// the closure; setns+close after fork is async-signal-safe (r1-cni.md
    /// "Join at spawn"). Called from container.rs start. Linux-validated only
    /// (contract §5). Transcribed from the fake's join_container_netns.
    #[cfg(target_os = "linux")]
    pub(crate) fn join_container_netns(
        &self,
        cmd: &mut std::process::Command,
        sandbox: &SandboxId,
    ) -> Result<()> {
        let netns_path: Option<String> = self
            .cache()
            .sandboxes
            .get(&sandbox.0)
            .and_then(|s| s.netns_path.clone());
        let Some(path_str) = netns_path else {
            return Ok(()); // host netns unchanged (probe-truthful)
        };
        let ns_fd = net::join_netns(std::path::Path::new(&path_str))
            .map_err(|e| BackendError::Internal(format!("start_container join_netns: {e}")))?;
        use std::os::unix::io::IntoRawFd;
        use std::os::unix::process::CommandExt;
        let raw_fd = ns_fd.into_raw_fd();
        // SAFETY: raw_fd is a valid O_RDONLY netns fd; setns+close are
        // async-signal-safe and allocate nothing.
        unsafe {
            cmd.pre_exec(move || {
                let rc = libc::setns(raw_fd, libc::CLONE_NEWNET);
                libc::close(raw_fd);
                if rc != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

// ── §D netns/CNI lifecycle (cfg(linux); probe-truthful no-op elsewhere) ───────
//
// The shapes below are TRANSCRIBED from lightr-cri-net (netns + chain). They
// are NOT a crate dependency (ADR-0017 firewall) — the syscalls are issued
// directly via `libc` so the crate takes no extra dep. The macOS/non-linux
// arms are honest no-ops: no kernel namespaces, no CNI → ip=None.

#[cfg(target_os = "linux")]
#[path = "sandbox_net.rs"]
pub(crate) mod net;

/// CNI setup: on linux, create+pin a netns and run the CNI chain → pod IP.
/// Returns (ip, netns_path). On non-linux or when CNI is unavailable, the
/// host-network fallback (None, None) — probe-truthful.
#[cfg(target_os = "linux")]
fn cni_setup(id: &SandboxId, cfg: &SandboxConfig) -> Result<(Option<String>, Option<String>)> {
    match net::cni_available() {
        Some(env) => net::setup(id, &env, &cfg.port_mappings)
            .map(|(ip, ns)| (ip, Some(ns)))
            .map_err(|e| BackendError::Internal(format!("CNI setup for sandbox {}: {e}", id.0))),
        None => Ok((None, None)), // unprivileged / no conflist → host-network fallback
    }
}

#[cfg(not(target_os = "linux"))]
fn cni_setup(_id: &SandboxId, _cfg: &SandboxConfig) -> Result<(Option<String>, Option<String>)> {
    Ok((None, None)) // no kernel namespaces on macOS/windows — probe-truthful
}

#[cfg(target_os = "linux")]
fn cni_teardown(id: &SandboxId, netns_path: &str) {
    net::teardown(id, netns_path);
}

#[cfg(not(target_os = "linux"))]
fn cni_teardown(_id: &SandboxId, _netns_path: &str) {}

#[cfg(target_os = "linux")]
fn cni_available() -> bool {
    net::cni_available().is_some()
}

#[cfg(not(target_os = "linux"))]
fn cni_available() -> bool {
    false
}

#[cfg(all(test, not(target_os = "linux")))]
#[path = "sandbox_tests.rs"]
mod tests;
