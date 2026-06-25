//! Sandbox state-machine tests — fully macOS-testable (the gate). The netns/CNI
//! RUNTIME is Linux-only and is NOT exercised here (probe-truthful: macOS has no
//! kernel namespaces → ip=None, network_ready=false); its real validation is
//! Linux CI / on-box (contract §5). Parallel-safe: each test owns a unique
//! tempdir home (atomic counter + nanos, no process-global mutation).

use crate::vocab::{
    ContainerConfig, ContainerState, SandboxConfig, SandboxFilter, SandboxId, SandboxState,
};
use crate::{CriBackend, LightrBackend};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn temp_home() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("lightr-cri-sb-{nanos}-{n}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn sb_cfg(name: &str) -> SandboxConfig {
    SandboxConfig {
        name: name.into(),
        uid: "uid".into(),
        namespace: "ns".into(),
        attempt: 0,
        labels: BTreeMap::new(),
        annotations: BTreeMap::new(),
        log_directory: String::new(),
        hostname: String::new(),
        host_network: false,
        dns: None,
        port_mappings: Vec::new(),
    }
}

fn ct_cfg(name: &str) -> ContainerConfig {
    ContainerConfig {
        name: name.into(),
        attempt: 0,
        image_ref: "img".into(),
        command: vec!["true".into()],
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

// ── lifecycle: run → Ready → stop → NotReady → remove → gone ──────────────────

#[test]
fn lifecycle_run_stop_remove() {
    let b = LightrBackend::new(temp_home());
    let id = b.run_sandbox(sb_cfg("pod")).unwrap();
    assert_eq!(b.sandbox_status(&id).unwrap().state, SandboxState::Ready);
    // macOS: no CNI → probe-truthful host-network fallback.
    let st = b.sandbox_status(&id).unwrap();
    assert!(st.ip.is_none() && st.netns_path.is_none());

    b.stop_sandbox(&id).unwrap();
    assert_eq!(b.sandbox_status(&id).unwrap().state, SandboxState::NotReady);

    b.remove_sandbox(&id).unwrap();
    assert!(b.sandbox_status(&id).is_err()); // gone → NotFound
}

#[test]
fn stop_is_idempotent() {
    let b = LightrBackend::new(temp_home());
    let id = b.run_sandbox(sb_cfg("pod")).unwrap();
    b.stop_sandbox(&id).unwrap();
    b.stop_sandbox(&id).unwrap(); // idempotent: no error
    assert_eq!(b.sandbox_status(&id).unwrap().state, SandboxState::NotReady);
}

#[test]
fn remove_is_idempotent_and_implies_stop() {
    let b = LightrBackend::new(temp_home());
    let id = b.run_sandbox(sb_cfg("pod")).unwrap();
    // remove directly from Ready (implies stop).
    b.remove_sandbox(&id).unwrap();
    b.remove_sandbox(&id).unwrap(); // idempotent on a gone sandbox
                                    // stop on a gone sandbox is also idempotent.
    b.stop_sandbox(&id).unwrap();
}

// ── list filters ─────────────────────────────────────────────────────────────

#[test]
fn list_filters_by_state_and_label() {
    let b = LightrBackend::new(temp_home());
    let mut c1 = sb_cfg("a");
    c1.labels.insert("team".into(), "core".into());
    let id1 = b.run_sandbox(c1).unwrap();
    let id2 = b.run_sandbox(sb_cfg("b")).unwrap();
    b.stop_sandbox(&id2).unwrap();

    // all
    assert_eq!(
        b.list_sandboxes(&SandboxFilter::default()).unwrap().len(),
        2
    );

    // by state Ready
    let ready = b
        .list_sandboxes(&SandboxFilter {
            state: Some(SandboxState::Ready),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, id1);

    // by label
    let mut sel = BTreeMap::new();
    sel.insert("team".into(), "core".into());
    let labeled = b
        .list_sandboxes(&SandboxFilter {
            label_selector: sel,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(labeled.len(), 1);
    assert_eq!(labeled[0].id, id1);

    // by id
    let by_id = b
        .list_sandboxes(&SandboxFilter {
            id: Some(id2.clone()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].id, id2);
}

// ── create_container gated on Ready sandbox ──────────────────────────────────

#[test]
fn create_container_requires_existing_sandbox() {
    let b = LightrBackend::new(temp_home());
    let err = b
        .create_container(&SandboxId("ghost".into()), ct_cfg("c"))
        .unwrap_err();
    assert!(matches!(err, crate::vocab::BackendError::NotFound(_)));
}

#[test]
fn create_container_refused_when_sandbox_not_ready() {
    let b = LightrBackend::new(temp_home());
    let id = b.run_sandbox(sb_cfg("pod")).unwrap();
    b.stop_sandbox(&id).unwrap(); // NotReady
    let err = b.create_container(&id, ct_cfg("c")).unwrap_err();
    assert!(matches!(
        err,
        crate::vocab::BackendError::FailedPrecondition(_)
    ));
}

#[test]
fn create_container_ok_when_sandbox_ready() {
    let b = LightrBackend::new(temp_home());
    let id = b.run_sandbox(sb_cfg("pod")).unwrap();
    let cid = b.create_container(&id, ct_cfg("c")).unwrap();
    assert_eq!(
        b.container_status(&cid).unwrap().state,
        ContainerState::Created
    );
}

// ── remove_sandbox cascades to its containers ────────────────────────────────

#[test]
fn remove_sandbox_cascades_to_containers() {
    let b = LightrBackend::new(temp_home());
    let id = b.run_sandbox(sb_cfg("pod")).unwrap();
    let cid = b.create_container(&id, ct_cfg("c")).unwrap();
    assert!(b.container_status(&cid).is_ok());

    b.remove_sandbox(&id).unwrap();
    // The container is stopped + removed with its sandbox.
    assert!(b.container_status(&cid).is_err());
    assert!(b.sandbox_status(&id).is_err());
}

// ── crash recovery: reopen → state re-derived from disk ──────────────────────

#[test]
fn crash_recovery_rederives_sandbox_state() {
    let home = temp_home();
    let id = {
        let b = LightrBackend::new(&home);
        let id = b.run_sandbox(sb_cfg("pod")).unwrap();
        b.stop_sandbox(&id).unwrap(); // persist NotReady
        id
    };
    // Reopen a fresh backend over the same home — state is rebuilt from disk.
    let b2 = LightrBackend::new(&home);
    assert_eq!(
        b2.sandbox_status(&id).unwrap().state,
        SandboxState::NotReady
    );
}

#[test]
fn crash_recovery_after_remove_is_gone() {
    let home = temp_home();
    let id = {
        let b = LightrBackend::new(&home);
        let id = b.run_sandbox(sb_cfg("pod")).unwrap();
        b.remove_sandbox(&id).unwrap();
        id
    };
    let b2 = LightrBackend::new(&home);
    assert!(b2.sandbox_status(&id).is_err());
}
