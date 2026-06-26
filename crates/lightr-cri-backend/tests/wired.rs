//! Integration tests for the WP-CRI-MVP wired methods (container / exec / stats
//! / image planes) over the real Lightr engine.
//!
//! Parallel-safe: every test gets its own unique tempdir `home` (atomic counter
//! plus nanos), and NO test mutates process-global state (no `set_var`, no cwd).
//! The image-pull-from-registry path is NOT exercised here because it needs the
//! network; the image plane is proven via the store-backed
//! status/list/remove/fs_info paths and an honest-error pull of an invalid ref.
//! The full critest/vectors run is the later WP-CRI-VECTORS.

use std::collections::BTreeMap;
use std::path::PathBuf;

use lightr_cri_backend::{
    BackendError, ContainerConfig, ContainerFilter, ContainerState, CriBackend, LightrBackend,
    SandboxConfig, SandboxId,
};

fn temp_home() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("lightr-cri-it-{nanos}-{n}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn cfg(name: &str, command: Vec<&str>) -> ContainerConfig {
    ContainerConfig {
        name: name.into(),
        attempt: 0,
        image_ref: "test-image".into(),
        command: command.into_iter().map(String::from).collect(),
        args: Vec::new(),
        working_dir: String::new(),
        envs: Vec::new(),
        mounts: Vec::new(),
        labels: BTreeMap::new(),
        annotations: BTreeMap::new(),
        log_path: String::new(),
        tty: false,
        stdin: false,
        security: None,
    }
}

/// Run a Ready **host_network** sandbox and return its id. create_container is
/// gated on a Ready sandbox (WP-CRI-SANDBOX). host_network ⇒ no pinned netns, so
/// these container/exec/stats/image-plane tests take the HOST-process path
/// deterministically (they don't test isolation). Post-#99 a netns'd pod would
/// fail-close on the non-hydratable `test-image`; host_network is the honest fit
/// for plane tests and removes the prior CNI-presence environment-dependence.
fn ready_sandbox(b: &LightrBackend) -> SandboxId {
    b.run_sandbox(SandboxConfig {
        name: "pod".into(),
        uid: "uid".into(),
        namespace: "ns".into(),
        attempt: 0,
        labels: BTreeMap::new(),
        annotations: BTreeMap::new(),
        log_directory: String::new(),
        hostname: String::new(),
        host_network: true,
        dns: None,
        port_mappings: Vec::new(),
    })
    .unwrap()
}

// ── container lifecycle: create → status → list → remove ─────────────────────

#[test]
fn container_lifecycle_create_status_list_remove() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);

    let id = b.create_container(&sb, cfg("c1", vec![])).unwrap();

    // Created state, before start.
    let st = b.container_status(&id).unwrap();
    assert_eq!(st.state, ContainerState::Created);
    assert_eq!(st.id, id);

    // list shows it; filter by state.
    let all = b.list_containers(&ContainerFilter::default()).unwrap();
    assert_eq!(all.len(), 1);
    let created = b
        .list_containers(&ContainerFilter {
            state: Some(ContainerState::Created),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(created.len(), 1);
    let running = b
        .list_containers(&ContainerFilter {
            state: Some(ContainerState::Running),
            ..Default::default()
        })
        .unwrap();
    assert!(running.is_empty());

    // remove (from Created) → gone; idempotent.
    b.remove_container(&id).unwrap();
    assert!(matches!(
        b.container_status(&id),
        Err(BackendError::NotFound(_))
    ));
    b.remove_container(&id).unwrap(); // idempotent
}

#[test]
fn start_then_keepalive_is_running_and_stops_with_grace() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    // Empty command ⇒ keep-alive `tail -f /dev/null` (transcribed from fake).
    let id = b.create_container(&sb, cfg("k", vec![])).unwrap();
    b.start_container(&id).unwrap();

    let st = b.container_status(&id).unwrap();
    assert_eq!(st.state, ContainerState::Running);
    assert!(st.started_at_nanos > 0);

    // graceful stop: SIGTERM then (if needed) SIGKILL; reaper records terminal.
    b.stop_container(&id, 2).unwrap();
    let st = b.container_status(&id).unwrap();
    assert_eq!(st.state, ContainerState::Exited);
    // Killed by a signal ⇒ non-zero exit ⇒ normalized reason "Error".
    assert_eq!(st.reason, "Error");

    // stop is idempotent on an exited container.
    b.stop_container(&id, 0).unwrap();
    b.remove_container(&id).unwrap();
}

#[test]
fn short_lived_command_exits_completed() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    let id = b.create_container(&sb, cfg("t", vec!["true"])).unwrap();
    b.start_container(&id).unwrap();

    // Wait for the reaper to land the terminal state.
    let mut exited = false;
    for _ in 0..200 {
        if b.container_status(&id).unwrap().state == ContainerState::Exited {
            exited = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(exited, "container did not reach Exited");
    let st = b.container_status(&id).unwrap();
    assert_eq!(st.exit_code, 0);
    assert_eq!(st.reason, "Completed");
}

#[test]
fn start_requires_created_state() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    let id = b.create_container(&sb, cfg("k", vec![])).unwrap();
    b.start_container(&id).unwrap();
    // Second start from Running ⇒ FailedPrecondition.
    assert!(matches!(
        b.start_container(&id),
        Err(BackendError::FailedPrecondition(_))
    ));
    b.remove_container(&id).unwrap();
}

#[test]
fn remove_force_stops_a_running_container() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    let id = b.create_container(&sb, cfg("k", vec![])).unwrap();
    b.start_container(&id).unwrap();
    assert_eq!(
        b.container_status(&id).unwrap().state,
        ContainerState::Running
    );
    // remove force-stops (SIGKILL) then deletes.
    b.remove_container(&id).unwrap();
    assert!(matches!(
        b.container_status(&id),
        Err(BackendError::NotFound(_))
    ));
}

// ── crash-only recovery: a restarted backend re-derives state from disk ──────

#[test]
fn state_survives_a_fresh_backend_over_the_same_home() {
    let home = temp_home();
    let id = {
        let b = LightrBackend::new(&home);
        let sb = ready_sandbox(&b);
        b.create_container(&sb, cfg("c", vec![])).unwrap()
    };
    // A brand-new backend over the same home rebuilds the cache from disk.
    let b2 = LightrBackend::new(&home);
    let st = b2.container_status(&id).unwrap();
    assert_eq!(st.state, ContainerState::Created);
}

// ── exec_sync: capture stdout / exit / timeout ───────────────────────────────

#[test]
fn exec_sync_captures_output_and_exit() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    let id = b.create_container(&sb, cfg("k", vec![])).unwrap();
    b.start_container(&id).unwrap();

    let r = b
        .exec_sync(&id, &["sh".into(), "-c".into(), "printf hello".into()], 5)
        .unwrap();
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.stdout, b"hello");

    let r = b
        .exec_sync(&id, &["sh".into(), "-c".into(), "exit 7".into()], 5)
        .unwrap();
    assert_eq!(r.exit_code, 7);

    b.remove_container(&id).unwrap();
}

#[test]
fn exec_sync_honors_timeout() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    let id = b.create_container(&sb, cfg("k", vec![])).unwrap();
    b.start_container(&id).unwrap();
    // sleep 30 with a 1s timeout ⇒ killed ⇒ Internal("exec timeout").
    let e = b.exec_sync(&id, &["sleep".into(), "30".into()], 1);
    assert!(matches!(e, Err(BackendError::Internal(m)) if m.contains("timeout")));
    b.remove_container(&id).unwrap();
}

#[test]
fn exec_sync_requires_running() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    let id = b.create_container(&sb, cfg("k", vec![])).unwrap();
    // Not started ⇒ FailedPrecondition.
    assert!(matches!(
        b.exec_sync(&id, &["true".into()], 5),
        Err(BackendError::FailedPrecondition(_))
    ));
}

// ── stats: real number on Linux, probe-truthful zero otherwise ───────────────

#[test]
fn stats_running_container_is_truthful() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    let id = b.create_container(&sb, cfg("k", vec![])).unwrap();
    b.start_container(&id).unwrap();

    let s = b.container_stats(&id).unwrap();
    assert_eq!(s.id, id);
    assert!(s.timestamp_nanos > 0);
    // On Linux the running keep-alive yields a real memory number; elsewhere it
    // is a probe-truthful zero. Either way the call returns a coherent record.
    #[cfg(target_os = "linux")]
    assert!(s.memory_working_set_bytes > 0);

    let all = b.list_container_stats(&ContainerFilter::default()).unwrap();
    assert_eq!(all.len(), 1);
    b.remove_container(&id).unwrap();
}

#[test]
fn stats_of_unstarted_container_is_zero() {
    let b = LightrBackend::new(temp_home());
    let sb = ready_sandbox(&b);
    let id = b.create_container(&sb, cfg("k", vec![])).unwrap();
    let s = b.container_stats(&id).unwrap();
    assert_eq!(s.cpu_usage_core_nanos, 0);
    assert_eq!(s.memory_working_set_bytes, 0);
    assert!(s.timestamp_nanos > 0);
}

// ── image plane: status / list / remove / fs_info over the real store ────────

#[test]
fn pull_invalid_ref_is_invalid_argument() {
    let b = LightrBackend::new(temp_home());
    assert!(matches!(
        b.pull_image(""),
        Err(BackendError::InvalidArgument(_))
    ));
    assert!(matches!(
        b.pull_image("has space"),
        Err(BackendError::InvalidArgument(_))
    ));
}

#[test]
fn image_status_absent_and_list_empty_then_fs_info_honest() {
    let b = LightrBackend::new(temp_home());
    // No images pulled ⇒ status None, list empty.
    assert!(b.image_status("busybox:latest").unwrap().is_none());
    assert!(b.list_images().unwrap().is_empty());
    // fs_info reads the real store usage — an empty store is honestly zero.
    let fs = b.image_fs_info().unwrap();
    assert!(fs.timestamp_nanos > 0);
    assert_eq!(fs.used_bytes, 0);
    assert_eq!(fs.inodes_used, 0);
    assert!(fs.mountpoint.contains("store"));
}

#[test]
fn remove_absent_image_is_idempotent_ok() {
    let b = LightrBackend::new(temp_home());
    // not-found → Ok (CRI law).
    b.remove_image("never-pulled").unwrap();
}
