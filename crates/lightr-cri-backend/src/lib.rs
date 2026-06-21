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
    /// Root directory under which all CRI backend state lives.
    home: PathBuf,
}

impl LightrBackend {
    /// Construct a backend rooted at `home`. Infallible: provisioning of the
    /// on-disk layout is the job of the methods that own it (later WPs).
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// The state root this backend was constructed with.
    pub fn home(&self) -> &std::path::Path {
        &self.home
    }
}

/// Fail-closed sentinel: every skeleton method returns this — an honest error,
/// never `todo!()`/`unimplemented!()`/`panic!`. The seam must compile and fail
/// closed everywhere (including the macOS + windows-cross compile lanes).
fn not_yet(method: &str, wp: &str) -> BackendError {
    BackendError::Internal(format!("not yet implemented: {method} — {wp}"))
}

impl CriBackend for LightrBackend {
    // sandbox plane — WP-CRI-SANDBOX
    fn run_sandbox(&self, _cfg: SandboxConfig) -> Result<SandboxId> {
        Err(not_yet("run_sandbox", "WP-CRI-SANDBOX"))
    }
    fn stop_sandbox(&self, _id: &SandboxId) -> Result<()> {
        Err(not_yet("stop_sandbox", "WP-CRI-SANDBOX"))
    }
    fn remove_sandbox(&self, _id: &SandboxId) -> Result<()> {
        Err(not_yet("remove_sandbox", "WP-CRI-SANDBOX"))
    }
    fn sandbox_status(&self, _id: &SandboxId) -> Result<SandboxStatus> {
        Err(not_yet("sandbox_status", "WP-CRI-SANDBOX"))
    }
    fn list_sandboxes(&self, _filter: &SandboxFilter) -> Result<Vec<SandboxStatus>> {
        Err(not_yet("list_sandboxes", "WP-CRI-SANDBOX"))
    }

    // container plane — WP-CRI-CONTAINER
    fn create_container(&self, _sandbox: &SandboxId, _cfg: ContainerConfig) -> Result<ContainerId> {
        Err(not_yet("create_container", "WP-CRI-CONTAINER"))
    }
    fn start_container(&self, _id: &ContainerId) -> Result<()> {
        Err(not_yet("start_container", "WP-CRI-CONTAINER"))
    }
    fn stop_container(&self, _id: &ContainerId, _grace_seconds: i64) -> Result<()> {
        Err(not_yet("stop_container", "WP-CRI-CONTAINER"))
    }
    fn remove_container(&self, _id: &ContainerId) -> Result<()> {
        Err(not_yet("remove_container", "WP-CRI-CONTAINER"))
    }
    fn container_status(&self, _id: &ContainerId) -> Result<ContainerStatus> {
        Err(not_yet("container_status", "WP-CRI-CONTAINER"))
    }
    fn list_containers(&self, _filter: &ContainerFilter) -> Result<Vec<ContainerStatus>> {
        Err(not_yet("list_containers", "WP-CRI-CONTAINER"))
    }
    fn container_stats(&self, _id: &ContainerId) -> Result<ContainerStatsRec> {
        Err(not_yet("container_stats", "WP-CRI-STATS"))
    }
    fn list_container_stats(&self, _filter: &ContainerFilter) -> Result<Vec<ContainerStatsRec>> {
        Err(not_yet("list_container_stats", "WP-CRI-STATS"))
    }

    // exec plane — WP-CRI-EXEC
    fn exec_sync(
        &self,
        _id: &ContainerId,
        _cmd: &[String],
        _timeout_seconds: i64,
    ) -> Result<ExecResult> {
        Err(not_yet("exec_sync", "WP-CRI-EXEC"))
    }

    // image plane — WP-CRI-IMAGE
    fn pull_image(&self, _image_ref: &str) -> Result<PulledImage> {
        Err(not_yet("pull_image", "WP-CRI-IMAGE"))
    }
    fn image_status(&self, _image_ref: &str) -> Result<Option<ImageRecord>> {
        Err(not_yet("image_status", "WP-CRI-IMAGE"))
    }
    fn list_images(&self) -> Result<Vec<ImageRecord>> {
        Err(not_yet("list_images", "WP-CRI-IMAGE"))
    }
    fn remove_image(&self, _image_ref: &str) -> Result<()> {
        Err(not_yet("remove_image", "WP-CRI-IMAGE"))
    }
    fn image_fs_info(&self) -> Result<FsInfo> {
        Err(not_yet("image_fs_info", "WP-CRI-IMAGE"))
    }

    // v1.1 streaming — WP-CRI-STREAM. Honest-error OVERRIDES (not the trait
    // defaults) so the skeleton's failure is attributed to its WP, not the
    // generic "v1.1 not implemented" default. `pull_image_with_auth` keeps the
    // trait default (delegates to pull_image, which fails closed above), and
    // `network_ready` keeps the trait default (false = probe-truthful: this
    // skeleton wires no CNI).
    fn open_exec(
        &self,
        _id: &ContainerId,
        _cmd: &[String],
        _tty: bool,
        _stdin: bool,
    ) -> Result<StreamSession> {
        Err(not_yet("open_exec", "WP-CRI-STREAM"))
    }
    fn open_attach(&self, _id: &ContainerId) -> Result<StreamSession> {
        Err(not_yet("open_attach", "WP-CRI-STREAM"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_constructs_and_keeps_home() {
        let b = LightrBackend::new("/tmp/lightr-cri-test-home");
        assert_eq!(b.home(), std::path::Path::new("/tmp/lightr-cri-test-home"));
    }

    #[test]
    fn methods_fail_closed_not_panic() {
        let b = LightrBackend::new("/tmp/lightr-cri-test-home");
        // A representative method from each plane returns the honest error.
        let e = b.run_sandbox(SandboxConfig {
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
        });
        match e {
            Err(BackendError::Internal(m)) => {
                assert!(m.contains("not yet implemented"), "msg: {m}");
                assert!(m.contains("run_sandbox"), "msg: {m}");
                assert!(m.contains("WP-CRI-SANDBOX"), "msg: {m}");
            }
            other => panic!("expected fail-closed Internal error, got {other:?}"),
        }
        assert!(matches!(
            b.pull_image("busybox"),
            Err(BackendError::Internal(_))
        ));
        // v1.1 streaming override also fails closed (not the generic default).
        assert!(matches!(
            b.open_exec(&ContainerId("c".into()), &[], false, false),
            Err(BackendError::Internal(_))
        ));
        // probe-truthful: no CNI wired → network not ready.
        assert!(!b.network_ready());
    }

    /// The skeleton is object-safe behind `dyn CriBackend` (the shell consumes
    /// it as a trait object; this guards object-safety at the seam).
    #[test]
    fn is_object_safe() {
        let b: Box<dyn CriBackend> = Box::new(LightrBackend::new("/tmp/x"));
        assert!(b.list_images().is_err());
    }
}
