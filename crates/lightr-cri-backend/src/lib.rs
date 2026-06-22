//! `lightr-cri-backend` — the CRI backend seam, in the hugr-lightr workspace.
//!
//! PROVENANCE. The `CriBackend` trait and EVERY vocabulary type in this crate
//! are TRANSCRIBED — wire-for-wire — from the frozen seam contract owned by the
//! `lightr-cri` repo:
//!   - trait + v1.1 default-provided methods: `lightr-cri/crates/lightr-cri-backend/src/lib.rs`
//!   - vocabulary types: `lightr-cri/crates/lightr-cri-backend/src/{lib.rs,vocab.rs}`
//!   - semantics: `lightr-cri/docs/contract/seam-contract-v1.1.md` (FROZEN 2026-06-12)
//!
//! This is NOT a git/path dependency on `lightr-cri` (ADR-0017 decision 3, the
//! house seam pattern). The transcribed seam is proven to match the contract
//! wire-for-wire later, by the shared conformance vectors (WP-CRI-VECTORS) —
//! never by a crate import. Drift is caught by those vectors.
//!
//! DEPENDENCY FIREWALL (ADR-0017 decision 5): this crate (and the whole
//! hugr-lightr workspace) NEVER takes tonic/prost/gRPC. Those belong only to
//! the future CRI shell, which lives behind this seam, not in front of it.
//!
//! `LightrBackend` is the real backend that fulfills the seam against the
//! hugr-lightr crates. Here it is a fail-closed SKELETON: every method returns
//! an honest `BackendError` ("not yet implemented") and NEVER panics. The MVP
//! WPs (WP-CRI-SANDBOX, -CONTAINER, -IMAGE, -EXEC, …) fill it in.

pub mod vocab;

// WP-CRI-MVP planes (split by concern, each <400 LOC). The trait impl below
// delegates to the inherent `_impl` methods defined in these modules. Streaming
// stays fail-closed here (WP-CRI-STREAM); the sandbox/pod plane is now wired
// (WP-CRI-SANDBOX).
mod container;
mod container_query;
mod exec;
mod images;
mod sandbox;
mod stats;
mod stream;
// Streaming I/O machinery (io-table, fan-out, fd primitives, waiters) — unix
// only (pty/pipes/signals are unix concepts; the plane fails closed elsewhere).
#[cfg(unix)]
mod stream_io;
mod util;

// Re-export the whole seam vocabulary at the crate root (house convention: a
// `pub mod` whose items the shell + later WPs consume must be re-exported, or
// the items are pub-in-private dead code under `clippy -D warnings`).
pub use vocab::{
    AuthConfig, BackendError, ContainerConfig, ContainerFilter, ContainerId, ContainerState,
    ContainerStatsRec, ContainerStatus, DnsConfig, ExecResult, ExitWaiter, FsInfo, ImageRecord,
    Mount, PortMapping, Protocol, PulledImage, Result, SandboxConfig, SandboxFilter, SandboxId,
    SandboxState, SandboxStatus, StreamSession,
};

use std::path::PathBuf;

/// The seam. Synchronous on purpose: the real backend (hugr-lightr crates)
/// is sync; the shell bridges via spawn_blocking. Object-safe.
///
/// State law (vectors encode this): sandbox Ready→NotReady (stop)→gone
/// (remove). `create_container` requires the sandbox Ready (else
/// FailedPrecondition). Container Created→Running→Exited; `start` only from Created;
/// `stop` from Running (→Exited) or no-op from Created/Exited; `remove`
/// refused (FailedPrecondition) while Running; removing a sandbox
/// stops+removes its containers. All transitions persist BEFORE the call
/// returns (crash-only law).
pub trait CriBackend: Send + Sync + 'static {
    // sandbox plane
    fn run_sandbox(&self, cfg: SandboxConfig) -> Result<SandboxId>;
    /// idempotent
    fn stop_sandbox(&self, id: &SandboxId) -> Result<()>;
    /// idempotent; implies stop; removes its containers
    fn remove_sandbox(&self, id: &SandboxId) -> Result<()>;
    fn sandbox_status(&self, id: &SandboxId) -> Result<SandboxStatus>;
    fn list_sandboxes(&self, filter: &SandboxFilter) -> Result<Vec<SandboxStatus>>;

    // container plane
    fn create_container(&self, sandbox: &SandboxId, cfg: ContainerConfig) -> Result<ContainerId>;
    fn start_container(&self, id: &ContainerId) -> Result<()>;
    /// idempotent
    fn stop_container(&self, id: &ContainerId, grace_seconds: i64) -> Result<()>;
    /// idempotent; only when not Running
    fn remove_container(&self, id: &ContainerId) -> Result<()>;
    fn container_status(&self, id: &ContainerId) -> Result<ContainerStatus>;
    fn list_containers(&self, filter: &ContainerFilter) -> Result<Vec<ContainerStatus>>;
    fn container_stats(&self, id: &ContainerId) -> Result<ContainerStatsRec>;
    fn list_container_stats(&self, filter: &ContainerFilter) -> Result<Vec<ContainerStatsRec>>;

    // exec plane (R0: sync only)
    fn exec_sync(
        &self,
        id: &ContainerId,
        cmd: &[String],
        timeout_seconds: i64,
    ) -> Result<ExecResult>;

    // image plane (lazy law: pull_image MUST NOT move file bytes)
    fn pull_image(&self, image_ref: &str) -> Result<PulledImage>;
    fn image_status(&self, image_ref: &str) -> Result<Option<ImageRecord>>;
    fn list_images(&self) -> Result<Vec<ImageRecord>>;
    /// idempotent: not-found → Ok; refuses InUse while referenced by a live container
    fn remove_image(&self, image_ref: &str) -> Result<()>;
    fn image_fs_info(&self) -> Result<FsInfo>;

    // v1.1 streaming methods — default impls so v1-only backends compile
    fn open_exec(
        &self,
        _id: &ContainerId,
        _cmd: &[String],
        _tty: bool,
        _stdin: bool,
    ) -> Result<StreamSession> {
        Err(BackendError::Internal("v1.1 not implemented".to_string()))
    }
    /// Attach to the container's live stdio (spawned with held pipes/pty).
    fn open_attach(&self, _id: &ContainerId) -> Result<StreamSession> {
        Err(BackendError::Internal("v1.1 not implemented".to_string()))
    }
    /// Auth-aware pull; default delegates to pull_image (auth ignored = fake-honest).
    fn pull_image_with_auth(
        &self,
        image_ref: &str,
        _auth: Option<&AuthConfig>,
    ) -> Result<PulledImage> {
        self.pull_image(image_ref)
    }
    /// Honest network readiness for the CRI `Status.NetworkReady` condition
    /// (probe-truthful law, contract §D). Default false: a backend that does
    /// not wire CNI must NOT claim the pod network is ready. The fake
    /// overrides this to reflect `cni_available()`.
    fn network_ready(&self) -> bool {
        false
    }
}

// ── LightrBackend — the real backend (fail-closed skeleton) ──────────────────

/// The CAS-native CRI backend over the hugr-lightr crates.
///
/// Fields are intentionally minimal here; later WPs add the store/engine
/// handles they need (e.g. an image plane handle in WP-CRI-IMAGE, a sandbox
/// state store in WP-CRI-SANDBOX). `home` roots all on-disk state (house
/// convention: inject the root, never read process-global cwd — keeps state
/// per-instance and tests parallel-safe).
pub struct LightrBackend {
    /// Root directory under which all CRI backend state lives. CRI records live
    /// under `<home>/cri/`; the CAS store lives under `<home>/store/`.
    home: PathBuf,
    /// In-memory container cache — a VIEW rebuilt from disk at construction
    /// (crash-only law). The disk under `<home>/cri/containers/` is the source
    /// of truth; a restarted backend re-derives state from it (ADR-0017).
    cache: std::sync::Arc<std::sync::Mutex<container::Cache>>,
    /// Side-table of LIVE stdio held by `start_container` for the streaming
    /// plane (WP-CRI-STREAM `open_attach`): pty master or fan-out + pipe handles
    /// keyed by container id. NOT persisted — the fds are valid only in this
    /// process (attach is unavailable after a restart; `open_attach` surfaces
    /// that honestly), so it is rebuilt empty on construction. unix-only: pty +
    /// OS pipes are unix concepts (the windows gate compiles but never runs the
    /// streaming plane).
    #[cfg(unix)]
    pub(crate) io_table: std::sync::Arc<
        std::sync::Mutex<std::collections::BTreeMap<String, stream_io::ContainerIo>>,
    >,
}

impl LightrBackend {
    /// Construct a backend rooted at `home`, provisioning the CRI on-disk layout
    /// and rebuilding the container cache from disk (re-adopting survivors,
    /// reconciling Running records whose process is gone — crash-only recovery).
    /// Infallible: a provisioning failure degrades to an empty cache rather than
    /// panicking (the seam must never panic); the first mutating call surfaces
    /// the real I/O error honestly.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let _ = std::fs::create_dir_all(home.join("cri").join("containers"));
        let _ = std::fs::create_dir_all(home.join("cri").join("images"));
        let _ = std::fs::create_dir_all(home.join("cri").join("sandboxes"));
        let backend = Self {
            home,
            cache: std::sync::Arc::new(std::sync::Mutex::new(container::Cache::default())),
            #[cfg(unix)]
            io_table: std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
        };
        // Crash-only recovery: rebuild both cache halves from disk on open.
        let mut cache = backend.load_container_cache();
        cache.sandboxes = backend.load_sandbox_cache();
        *backend.cache.lock().unwrap() = cache;
        backend
    }

    /// The state root this backend was constructed with.
    pub fn home(&self) -> &std::path::Path {
        &self.home
    }

    /// Directory holding per-container record sidecars.
    pub(crate) fn containers_dir(&self) -> PathBuf {
        self.home.join("cri").join("containers")
    }

    /// Directory holding per-image CRI record sidecars.
    pub(crate) fn images_dir(&self) -> PathBuf {
        self.home.join("cri").join("images")
    }

    /// Directory holding per-sandbox CRI record sidecars.
    pub(crate) fn sandboxes_dir(&self) -> PathBuf {
        self.home.join("cri").join("sandboxes")
    }
}

impl CriBackend for LightrBackend {
    // sandbox plane — WP-CRI-SANDBOX (wired: state machine + cfg(linux) netns/CNI)
    fn run_sandbox(&self, cfg: SandboxConfig) -> Result<SandboxId> {
        self.run_sandbox_impl(cfg)
    }
    fn stop_sandbox(&self, id: &SandboxId) -> Result<()> {
        self.stop_sandbox_impl(id)
    }
    fn remove_sandbox(&self, id: &SandboxId) -> Result<()> {
        self.remove_sandbox_impl(id)
    }
    fn sandbox_status(&self, id: &SandboxId) -> Result<SandboxStatus> {
        self.sandbox_status_impl(id)
    }
    fn list_sandboxes(&self, filter: &SandboxFilter) -> Result<Vec<SandboxStatus>> {
        self.list_sandboxes_impl(filter)
    }

    // container plane — WP-CRI-MVP (wired to the engine via inherent methods)
    fn create_container(&self, sandbox: &SandboxId, cfg: ContainerConfig) -> Result<ContainerId> {
        self.create_container_impl(sandbox, cfg)
    }
    fn start_container(&self, id: &ContainerId) -> Result<()> {
        self.start_container_impl(id)
    }
    fn stop_container(&self, id: &ContainerId, grace_seconds: i64) -> Result<()> {
        self.stop_container_impl(id, grace_seconds)
    }
    fn remove_container(&self, id: &ContainerId) -> Result<()> {
        self.remove_container_impl(id)
    }
    fn container_status(&self, id: &ContainerId) -> Result<ContainerStatus> {
        self.container_status_impl(id)
    }
    fn list_containers(&self, filter: &ContainerFilter) -> Result<Vec<ContainerStatus>> {
        self.list_containers_impl(filter)
    }
    fn container_stats(&self, id: &ContainerId) -> Result<ContainerStatsRec> {
        self.container_stats_impl(id)
    }
    fn list_container_stats(&self, filter: &ContainerFilter) -> Result<Vec<ContainerStatsRec>> {
        self.list_container_stats_impl(filter)
    }

    // exec plane — WP-CRI-MVP
    fn exec_sync(
        &self,
        id: &ContainerId,
        cmd: &[String],
        timeout_seconds: i64,
    ) -> Result<ExecResult> {
        self.exec_sync_impl(id, cmd, timeout_seconds)
    }

    // image plane — WP-CRI-MVP (wired to lightr_oci + lightr_store)
    fn pull_image(&self, image_ref: &str) -> Result<PulledImage> {
        self.pull_image_impl(image_ref)
    }
    fn image_status(&self, image_ref: &str) -> Result<Option<ImageRecord>> {
        self.image_status_impl(image_ref)
    }
    fn list_images(&self) -> Result<Vec<ImageRecord>> {
        self.list_images_impl()
    }
    fn remove_image(&self, image_ref: &str) -> Result<()> {
        self.remove_image_impl(image_ref)
    }
    fn image_fs_info(&self) -> Result<FsInfo> {
        self.image_fs_info_impl()
    }

    // network_ready — WP-CRI-SANDBOX. Overrides the trait default (false) to
    // reflect REAL CNI state (probe-truthful, contract §D): true iff CNI is
    // wired+available, which on macOS / unprivileged is false (host-network).
    fn network_ready(&self) -> bool {
        self.network_ready_impl()
    }

    // v1.1 streaming — WP-CRI-STREAM. Wired to the inherent impls in `stream`:
    // open_exec spawns `cmd` (piped or pty stdio) and returns a real waiter;
    // open_attach registers a fan-out sink (or dups the pty master) against the
    // running container's live stdio held by `start_container`. unix-only (fail
    // closed on non-unix). `pull_image_with_auth` keeps the trait default.
    fn open_exec(
        &self,
        id: &ContainerId,
        cmd: &[String],
        tty: bool,
        stdin: bool,
    ) -> Result<StreamSession> {
        self.open_exec_impl(id, cmd, tty, stdin)
    }
    fn open_attach(&self, id: &ContainerId) -> Result<StreamSession> {
        self.open_attach_impl(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parallel-safe unique tempdir (atomic counter + nanos, no set_var).
    fn temp_home() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("lightr-cri-lib-{nanos}-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn new_constructs_and_keeps_home() {
        let home = temp_home();
        let b = LightrBackend::new(&home);
        assert_eq!(b.home(), home.as_path());
        // The CRI layout is provisioned on construction (crash-only root).
        assert!(home.join("cri").join("containers").is_dir());
    }

    /// Both planes are now WIRED: run_sandbox creates a Ready sandbox (on macOS
    /// no CNI → ip=None, probe-truthful); streaming (WP-CRI-STREAM) fails closed
    /// with a faithful `NotFound` on a missing container and never panics.
    #[test]
    fn sandbox_runs_and_streaming_fails_closed() {
        let b = LightrBackend::new(temp_home());
        let id = b
            .run_sandbox(SandboxConfig {
                name: "s".into(),
                uid: "u".into(),
                namespace: "ns".into(),
                attempt: 0,
                labels: Default::default(),
                annotations: Default::default(),
                log_directory: String::new(),
                hostname: String::new(),
                host_network: false,
                dns: None,
                port_mappings: Vec::new(),
            })
            .expect("run_sandbox succeeds");
        let st = b.sandbox_status(&id).unwrap();
        assert_eq!(st.state, SandboxState::Ready);
        // macOS gate: no CNI → no pod IP (probe-truthful).
        assert!(st.ip.is_none());
        // Streaming is wired: a missing container fails closed with NotFound
        // (the seam never panics), and open_attach on the same id likewise.
        assert!(matches!(
            b.open_exec(&ContainerId("c".into()), &["true".into()], false, false),
            Err(BackendError::NotFound(_))
        ));
        assert!(matches!(
            b.open_attach(&ContainerId("c".into())),
            Err(BackendError::NotFound(_))
        ));
        // probe-truthful: no CNI wired → network not ready.
        assert!(!b.network_ready());
    }

    /// Object-safe behind `dyn CriBackend` (the shell consumes it as a trait
    /// object). list_images on an empty store is Ok(empty) now it is wired.
    #[test]
    fn is_object_safe() {
        let b: Box<dyn CriBackend> = Box::new(LightrBackend::new(temp_home()));
        assert!(b.list_images().unwrap().is_empty());
    }
}
