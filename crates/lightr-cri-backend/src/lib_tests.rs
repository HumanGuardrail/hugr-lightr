//! LightrBackend/CriBackend unit tests, split out of lib.rs for the <=400-LOC
//! godfile invariant (verbatim; `tests` stays a child of the crate root so
//! `use super::*` resolves unchanged).

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
    // Pod IP is CNI-dependent: None on the macOS gate / no CNI (probe-truthful),
    // Some(addr) on Linux when a CNI conflist+plugins are present — validated in
    // the linux-validation lane, where CNI ADD assigns e.g. 10.88.0.x. Both are
    // correct; an assigned IP must be a non-empty, parseable address.
    if let Some(ip) = st.ip.as_deref() {
        assert!(
            !ip.is_empty() && ip.parse::<std::net::IpAddr>().is_ok(),
            "CNI-assigned pod IP must be a valid address, got {ip:?}"
        );
    }
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
    // probe-truthful + CNI-aware: `network_ready()` is the `cni_available()`
    // probe, and a pod IP is assigned iff CNI was available at setup — so the
    // two must AGREE. No CNI (macOS gate / unprivileged) → both false; root +
    // CNI conflist (the linux-validation lane) → both true. Asserting the
    // invariant (not a hard-coded false) is what the deeper Linux run exposed.
    assert_eq!(
        b.network_ready(),
        st.ip.is_some(),
        "network_ready() ({}) must agree with whether CNI assigned a pod IP ({:?})",
        b.network_ready(),
        st.ip
    );
}

/// Object-safe behind `dyn CriBackend` (the shell consumes it as a trait
/// object). list_images on an empty store is Ok(empty) now it is wired.
#[test]
fn is_object_safe() {
    let b: Box<dyn CriBackend> = Box::new(LightrBackend::new(temp_home()));
    assert!(b.list_images().unwrap().is_empty());
}
