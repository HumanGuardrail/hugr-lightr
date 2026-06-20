//! RC-SEAM-FREEZE tests — the per-field apply dispatch is WIRED and every
//! applier is a no-op today (behaviour-preserving). Split from `apply_cfg.rs`
//! via `#[cfg(test)] #[path] mod tests;` (house convention, godfile cap).

use super::{apply_run_config_ondisk, apply_run_config_spec};
use crate::run::types::{RunSpec, SpecOnDisk};
use std::process::Command;

/// The `RunSpec` dispatch entry point is reachable and a no-default-field run
/// leaves the `Command`'s program/args/cwd untouched — today's behaviour.
#[test]
fn run_config_spec_dispatch_is_noop_on_default() {
    let spec = RunSpec {
        cwd: std::path::PathBuf::from("/tmp/x"),
        command: vec!["/bin/echo".to_string(), "hi".to_string()],
        ..Default::default()
    };
    let mut cmd = Command::new("/bin/echo");
    cmd.arg("hi");
    // Reachable + returns. Stubs are no-ops, so this neither panics nor mutates
    // observable program/args (a no-op applier cannot remove them).
    apply_run_config_spec(&spec, &mut cmd);
    assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/echo"));
    let args: Vec<_> = cmd.get_args().collect();
    assert_eq!(args, vec![std::ffi::OsStr::new("hi")]);
}

/// The `SpecOnDisk` dispatch entry point (detached supervisor path) is reachable
/// on a default-field persisted spec and is likewise a no-op.
#[test]
fn run_config_ondisk_dispatch_is_noop_on_default() {
    let spec = SpecOnDisk {
        command: vec!["/bin/true".to_string()],
        ..Default::default()
    };
    let mut cmd = Command::new("/bin/true");
    apply_run_config_ondisk(&spec, &mut cmd);
    assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/true"));
    assert_eq!(cmd.get_args().count(), 0);
}

/// Both dispatch entry points are reachable with the new RC carry-fields SET —
/// the appliers consume every field without panicking (still no-ops today).
#[test]
fn run_config_dispatch_reachable_with_fields_set() {
    let spec = RunSpec {
        cwd: std::path::PathBuf::from("/tmp/y"),
        command: vec!["/bin/true".to_string()],
        hostname: Some("h".to_string()),
        labels: vec![("k".to_string(), "v".to_string())],
        cap_add: vec!["NET_ADMIN".to_string()],
        cap_drop: vec!["MKNOD".to_string()],
        privileged: true,
        tty: true,
        init: true,
        read_only: true,
        oom_score_adj: Some(-500),
        pids_limit: Some(64),
        shm_size: Some(67_108_864),
        ..Default::default()
    };
    let mut cmd = Command::new("/bin/true");
    apply_run_config_spec(&spec, &mut cmd);

    let on_disk = SpecOnDisk {
        command: vec!["/bin/true".to_string()],
        hostname: Some("h".to_string()),
        labels: vec![("k".to_string(), "v".to_string())],
        cap_add: vec!["NET_ADMIN".to_string()],
        cap_drop: vec!["MKNOD".to_string()],
        privileged: true,
        tty: true,
        init: true,
        read_only: true,
        oom_score_adj: Some(-500),
        pids_limit: Some(64),
        shm_size: Some(67_108_864),
        ..Default::default()
    };
    let mut cmd2 = Command::new("/bin/true");
    apply_run_config_ondisk(&on_disk, &mut cmd2);
    // No assertion on side effects (stubs are no-ops); the point is reachability
    // of every applier slot with a non-default value, so the seam is exercised.
}
