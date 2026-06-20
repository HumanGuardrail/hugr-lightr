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
