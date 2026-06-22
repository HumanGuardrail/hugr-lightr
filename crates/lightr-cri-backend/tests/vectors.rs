//! WP-CRI-VECTORS — the seam's red→green acceptance bar.
//!
//! Proves `LightrBackend` passes the SAME shared conformance vectors the
//! lightr-cri fake passes, for the IMPLEMENTED methods (container / exec /
//! image / stats). The runner + the vector corpus are TRANSCRIBED from
//! `lightr-cri @ seam-contract-v1.1` (wire-level seam proof, NOT a git/path dep
//! — drift is caught HERE; see `vectors/data.rs` + `vectors/runner.rs`).
//!
//! GREENLIST DISCIPLINE (fail-closed, never silent): every vector is either RUN
//! or gated out + LOGGED with its reason (see `Category` in `vectors/data.rs`).
//! Sandbox + streaming + log-file + image-content-pull vectors are DEFERRED
//! because the underlying methods are fail-closed or network-bound in the MVP
//! backend (WP-CRI-SANDBOX / WP-CRI-STREAM). The RUN set drives the REAL
//! implemented container/exec/image/stats methods; the sandbox PREFIX of a
//! lifecycle vector is satisfied by an explicit, clearly-labeled TEST SCAFFOLD
//! (`ScaffoldBackend`) that adds only in-memory sandbox bookkeeping and
//! delegates every other call straight to the real `LightrBackend`. The
//! scaffold is test-only and touches NO `src/`: it lets the shared lifecycle
//! vectors exercise the real methods without the (deferred) sandbox plane.
//!
//! Parallel-safe: each vector runs over its own unique tempdir `home` (atomic
//! counter + nanos); no `set_var`, no shared global.

#[path = "vectors/data.rs"]
mod data;
#[path = "vectors/runner.rs"]
mod runner;
#[path = "vectors/runner2.rs"]
mod runner2;
#[path = "vectors/step.rs"]
mod step;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use lightr_cri_backend::{
    BackendError, ContainerConfig, ContainerFilter, ContainerId, ContainerStatsRec,
    ContainerStatus, CriBackend, ExecResult, FsInfo, ImageRecord, LightrBackend, PulledImage,
    Result, SandboxConfig, SandboxFilter, SandboxId, SandboxState, SandboxStatus,
};

use data::Category;
use runner::{BackendFactory, Vector};

// ── unique tempdir home (parallel-safe; no set_var) ──────────────────────────

fn temp_home() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("lightr-cri-vec-{nanos}-{n}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// ── ScaffoldBackend — REAL LightrBackend + in-memory sandbox bookkeeping ─────

/// A test-only `CriBackend` that satisfies the (deferred, fail-closed) sandbox
/// plane with minimal in-memory state and delegates EVERY container / exec /
/// image / stats call to the real `LightrBackend`. This is NOT a backend
/// change (it lives only in `tests/`); it is the scaffold that lets the shared
/// lifecycle vectors drive the real implemented methods while the sandbox plane
/// is still WP-CRI-SANDBOX. Sandbox semantics here are intentionally minimal —
/// the vectors that ASSERT sandbox behavior are DEFERRED, not run through this.
struct ScaffoldBackend {
    inner: LightrBackend,
    sandboxes: Mutex<BTreeMap<String, SandboxRec>>,
}

struct SandboxRec {
    config: SandboxConfig,
    state: SandboxState,
    created_at_nanos: i64,
}

impl ScaffoldBackend {
    fn new(home: PathBuf) -> Self {
        Self {
            inner: LightrBackend::new(home),
            sandboxes: Mutex::new(BTreeMap::new()),
        }
    }
    fn reopen(home: PathBuf) -> Self {
        // Crash-recovery: the real backend re-derives container/image state from
        // disk; the scaffold's in-memory sandbox map is not persisted, so the
        // DeferSandbox vectors (which alone assert post-reopen sandbox state)
        // never run through the scaffold. A reopened scaffold starts with an
        // empty sandbox map — sufficient for the RUN set, which re-runs from
        // fresh() and never reopens.
        Self::new(home)
    }
}

impl CriBackend for ScaffoldBackend {
    // sandbox plane — TEST SCAFFOLD (in-memory)
    fn run_sandbox(&self, cfg: SandboxConfig) -> Result<SandboxId> {
        let id = format!("sb-scaffold-{}", self.sandboxes.lock().unwrap().len());
        self.sandboxes.lock().unwrap().insert(
            id.clone(),
            SandboxRec {
                config: cfg,
                state: SandboxState::Ready,
                created_at_nanos: now_nanos(),
            },
        );
        Ok(SandboxId(id))
    }
    fn stop_sandbox(&self, id: &SandboxId) -> Result<()> {
        if let Some(r) = self.sandboxes.lock().unwrap().get_mut(&id.0) {
            r.state = SandboxState::NotReady;
        }
        Ok(())
    }
    fn remove_sandbox(&self, id: &SandboxId) -> Result<()> {
        // Cascade: stop+remove every container in this sandbox (contract law),
        // delegated to the REAL backend so the lifecycle vectors that remove a
        // running sandbox exercise the real force-stop+remove path.
        let cids: Vec<ContainerId> = self
            .inner
            .list_containers(&ContainerFilter {
                sandbox: Some(id.clone()),
                ..Default::default()
            })?
            .into_iter()
            .map(|s| s.id)
            .collect();
        for cid in cids {
            self.inner.remove_container(&cid)?;
        }
        self.sandboxes.lock().unwrap().remove(&id.0);
        Ok(())
    }
    fn sandbox_status(&self, id: &SandboxId) -> Result<SandboxStatus> {
        let guard = self.sandboxes.lock().unwrap();
        let r = guard
            .get(&id.0)
            .ok_or_else(|| BackendError::NotFound(format!("sandbox {}", id.0)))?;
        Ok(SandboxStatus {
            id: id.clone(),
            config: r.config.clone(),
            state: r.state,
            created_at_nanos: r.created_at_nanos,
            ip: None,
            netns_path: None,
        })
    }
    fn list_sandboxes(&self, _filter: &SandboxFilter) -> Result<Vec<SandboxStatus>> {
        let ids: Vec<SandboxId> = self
            .sandboxes
            .lock()
            .unwrap()
            .keys()
            .map(|k| SandboxId(k.clone()))
            .collect();
        ids.iter().map(|id| self.sandbox_status(id)).collect()
    }

    // container / exec / image / stats — DELEGATE to the REAL backend
    fn create_container(&self, sandbox: &SandboxId, cfg: ContainerConfig) -> Result<ContainerId> {
        self.inner.create_container(sandbox, cfg)
    }
    fn start_container(&self, id: &ContainerId) -> Result<()> {
        self.inner.start_container(id)
    }
    fn stop_container(&self, id: &ContainerId, grace_seconds: i64) -> Result<()> {
        self.inner.stop_container(id, grace_seconds)
    }
    fn remove_container(&self, id: &ContainerId) -> Result<()> {
        self.inner.remove_container(id)
    }
    fn container_status(&self, id: &ContainerId) -> Result<ContainerStatus> {
        self.inner.container_status(id)
    }
    fn list_containers(&self, filter: &ContainerFilter) -> Result<Vec<ContainerStatus>> {
        self.inner.list_containers(filter)
    }
    fn container_stats(&self, id: &ContainerId) -> Result<ContainerStatsRec> {
        self.inner.container_stats(id)
    }
    fn list_container_stats(&self, filter: &ContainerFilter) -> Result<Vec<ContainerStatsRec>> {
        self.inner.list_container_stats(filter)
    }
    fn exec_sync(&self, id: &ContainerId, cmd: &[String], t: i64) -> Result<ExecResult> {
        self.inner.exec_sync(id, cmd, t)
    }
    fn pull_image(&self, image_ref: &str) -> Result<PulledImage> {
        self.inner.pull_image(image_ref)
    }
    fn image_status(&self, image_ref: &str) -> Result<Option<ImageRecord>> {
        self.inner.image_status(image_ref)
    }
    fn list_images(&self) -> Result<Vec<ImageRecord>> {
        self.inner.list_images()
    }
    fn remove_image(&self, image_ref: &str) -> Result<()> {
        self.inner.remove_image(image_ref)
    }
    fn image_fs_info(&self) -> Result<FsInfo> {
        self.inner.image_fs_info()
    }
}

fn now_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

// ── BackendFactory over the scaffolded real backend ──────────────────────────

/// `fresh()` = new tempdir home; `reopen()` = `LightrBackend::new(same_home)`
/// (crash-only: container/image state re-derives from disk).
struct LightrFactory {
    home: Mutex<PathBuf>,
}

impl LightrFactory {
    fn new() -> Self {
        Self {
            home: Mutex::new(temp_home()),
        }
    }
}

impl BackendFactory for LightrFactory {
    fn fresh(&self) -> Box<dyn CriBackend> {
        let home = temp_home();
        *self.home.lock().unwrap() = home.clone();
        Box::new(ScaffoldBackend::new(home))
    }
    fn reopen(&self) -> Box<dyn CriBackend> {
        let home = self.home.lock().unwrap().clone();
        Box::new(ScaffoldBackend::reopen(home))
    }
}

// ── The acceptance test: RUN the implemented-method vectors, LOG the deferred ─

#[test]
fn conformance_vectors_prove_the_mvp_backend() {
    let factory = LightrFactory::new();

    let mut run_pass = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut deferred: BTreeMap<&'static str, Vec<&'static str>> = BTreeMap::new();

    for def in data::vectors() {
        if def.category != Category::RunLifecycle {
            let reason = match def.category {
                Category::DeferSandbox => "sandbox-plane semantics (fail-closed, WP-CRI-SANDBOX)",
                Category::DeferStream => "streaming open_exec (fail-closed, WP-CRI-STREAM)",
                Category::DeferLog => "CRI log file (needs sandbox log_directory, WP-CRI-SANDBOX)",
                Category::DeferNet => "image-content pull (needs a live OCI registry)",
                Category::RunLifecycle => unreachable!(),
            };
            deferred.entry(reason).or_default().push(def.name);
            continue;
        }
        let vector: Vector = serde_json::from_str(def.json)
            .unwrap_or_else(|e| panic!("transcribed vector {} failed to parse: {e}", def.name));
        match runner::run_vector(&factory, &vector) {
            Ok(()) => run_pass += 1,
            Err(msg) => failures.push(msg),
        }
    }

    // GREENLIST log — never a silent skip.
    eprintln!("── WP-CRI-VECTORS GREENLIST ───────────────────────────────");
    eprintln!("RUN (implemented container/exec/image/stats): {run_pass} passed");
    let deferred_total: usize = deferred.values().map(Vec::len).sum();
    eprintln!("DEFERRED (gated out, logged): {deferred_total}");
    for (reason, names) in &deferred {
        eprintln!("  [{}] {}: {}", names.len(), reason, names.join(", "));
    }
    eprintln!("───────────────────────────────────────────────────────────");

    if !failures.is_empty() {
        for f in &failures {
            eprintln!("FAILED: {f}");
        }
        panic!("{} RUN vector(s) failed (see above)", failures.len());
    }

    // Lock the proven count so an accidental re-classification (e.g. silently
    // dropping a vector to "deferred") is caught by the gate.
    assert_eq!(run_pass, 12, "expected 12 RunLifecycle vectors to RUN+PASS");
    assert_eq!(deferred_total, 17, "expected 17 deferred vectors, logged");
}
