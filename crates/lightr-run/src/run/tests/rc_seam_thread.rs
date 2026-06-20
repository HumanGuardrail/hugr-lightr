//! RC-SEAM-FREEZE: the new RC carry-fields thread RunSpec → SpecOnDisk through
//! the real `spawn_detached` path and land in spec.json — the persisted shape
//! the detached supervisor reads back to drive the apply seam. Behaviour is
//! preserved (the supervisor still just runs the child); this asserts only the
//! threading, not any apply effect (the appliers are no-ops in the freeze).
#![cfg(test)]

use crate::run::paths::read_spec_on_disk;
use crate::run::spawn::spawn_detached;
use crate::run::stop::stop;
use crate::run::types::RunSpec;
use lightr_store::Store;

fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = super::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("LIGHTR_HOME", home.path());
    (home, guard)
}

#[test]
fn rc_seam_fields_thread_runspec_to_spec_json() {
    let (home, _guard) = isolated_home();
    let cwd = tempfile::tempdir().unwrap();
    let store = Store::open(home.path().join("store")).expect("store open");

    let spec = RunSpec {
        cwd: cwd.path().to_path_buf(),
        command: vec!["sleep".to_string(), "30".to_string()],
        hostname: Some("host-z".to_string()),
        labels: vec![("env".to_string(), "test".to_string())],
        cap_add: vec!["NET_ADMIN".to_string()],
        cap_drop: vec!["MKNOD".to_string()],
        privileged: true,
        tty: true,
        init: true,
        read_only: true,
        oom_score_adj: Some(-250),
        pids_limit: Some(128),
        shm_size: Some(33_554_432),
        ..Default::default()
    };

    let handle = spawn_detached(&spec, &store).expect("spawn_detached");
    let run_dir = handle.dir.clone();

    // The supervisor writes spec.json before forking the child; give it a moment.
    std::thread::sleep(std::time::Duration::from_millis(300));

    let on_disk = read_spec_on_disk(&run_dir).expect("read spec.json");
    assert_eq!(on_disk.hostname.as_deref(), Some("host-z"));
    assert_eq!(
        on_disk.labels,
        vec![("env".to_string(), "test".to_string())]
    );
    assert_eq!(on_disk.cap_add, vec!["NET_ADMIN".to_string()]);
    assert_eq!(on_disk.cap_drop, vec!["MKNOD".to_string()]);
    assert!(on_disk.privileged && on_disk.tty && on_disk.init && on_disk.read_only);
    assert_eq!(on_disk.oom_score_adj, Some(-250));
    assert_eq!(on_disk.pids_limit, Some(128));
    assert_eq!(on_disk.shm_size, Some(33_554_432));

    // Clean up the detached sleeper.
    let _ = stop(&run_dir, 1);
}
