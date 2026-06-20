//! Tests for SpecOnDisk serde back-compat: legacy JSON defaults + vz roundtrip.
#![cfg(test)]

use crate::run::paths::{read_spec_on_disk, write_spec_json};
use crate::run::types::SpecOnDisk;

#[test]
fn spec_on_disk_legacy_json_defaults_to_native() {
    let legacy = r#"{
        "cwd": "/w", "command": ["sleep","1"], "env_keys": [],
        "mounts": [], "detached": true, "created_at_unix": 1
    }"#;
    let spec: SpecOnDisk = serde_json::from_str(legacy).expect("legacy spec parses");
    assert_eq!(spec.engine, "native", "missing engine ⇒ native branch");
    assert!(spec.rootfs_ref.is_none(), "missing rootfs_ref ⇒ None");
    assert!(
        spec.ports.is_empty(),
        "missing ports ⇒ empty (existing default)"
    );
}

/// A vz container spec roundtrips through write/read with engine + rootfs_ref
/// preserved — what the supervisor reads to select the vz branch.
#[test]
fn spec_on_disk_vz_roundtrip_preserves_engine_and_rootfs() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SpecOnDisk {
        cwd: "/w".to_string(),
        command: vec!["sh".to_string()],
        env_keys: vec![],
        mounts: vec![],
        detached: true,
        created_at_unix: 1,
        ports: vec![(18080, 80)],
        engine: "vz".to_string(),
        rootfs_ref: Some("alpine".to_string()),
        env: vec![],
        ..Default::default()
    };
    write_spec_json(dir.path(), &spec).expect("write");
    let back = read_spec_on_disk(dir.path()).expect("read");
    assert_eq!(back.engine, "vz");
    assert_eq!(back.rootfs_ref.as_deref(), Some("alpine"));
    assert_eq!(back.ports, vec![(18080, 80)]);
}

// ── WP-RC-WORKDIR: effective_cwd + resolve_workdir ─────────────────────────

/// `RunSpec::effective_cwd`: no `-w` ⇒ `cwd` unchanged (behavior-preserving);
/// a relative `-w` joins under `cwd`; an absolute `-w` replaces (PathBuf::join /
/// Docker absolute-WORKDIR semantics).
#[test]
fn effective_cwd_resolves_workdir() {
    use crate::run::types::RunSpec;
    let base = std::path::PathBuf::from("/base/run");

    let none = RunSpec {
        cwd: base.clone(),
        workdir: None,
        ..Default::default()
    };
    assert_eq!(none.effective_cwd(), base, "no -w ⇒ cwd unchanged");

    let rel = RunSpec {
        cwd: base.clone(),
        workdir: Some("sub/wd".to_string()),
        ..Default::default()
    };
    assert_eq!(
        rel.effective_cwd(),
        base.join("sub/wd"),
        "relative -w joins"
    );

    let abs = RunSpec {
        cwd: base.clone(),
        workdir: Some("/abs/wd".to_string()),
        ..Default::default()
    };
    assert_eq!(
        abs.effective_cwd(),
        std::path::PathBuf::from("/abs/wd"),
        "absolute -w replaces"
    );
}

/// `resolve_workdir`: `None` ⇒ base returned, NO directory created (the no-`-w`
/// path must not touch the filesystem). `Some(rel)` ⇒ the dir is auto-created
/// (Docker creates WORKDIR) and the joined path returned.
#[test]
fn resolve_workdir_creates_dir_only_when_set() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();

    // None ⇒ base, no mkdir, no new entries under base.
    let got = crate::run::spawn::resolve_workdir(base, None).expect("resolve none");
    assert_eq!(got, base);
    let count = std::fs::read_dir(base).unwrap().count();
    assert_eq!(count, 0, "no -w must not create any directory");

    // Some(rel) ⇒ created + returned.
    let got = crate::run::spawn::resolve_workdir(base, Some("a/b/c")).expect("resolve some");
    assert_eq!(got, base.join("a/b/c"));
    assert!(got.is_dir(), "-w must auto-create the workdir recursively");
}
