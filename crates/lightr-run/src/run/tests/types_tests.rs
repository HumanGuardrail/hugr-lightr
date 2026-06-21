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

// RC-SEAM-FREEZE: a spec.json written before the new RC carry-fields existed
// still parses, with each new field at its no-op default (back-compat).
#[test]
fn spec_on_disk_legacy_json_defaults_rc_seam_fields() {
    let legacy = r#"{
        "cwd": "/w", "command": ["sleep","1"], "env_keys": [],
        "mounts": [], "detached": true, "created_at_unix": 1
    }"#;
    let spec: SpecOnDisk = serde_json::from_str(legacy).expect("legacy spec parses");
    assert!(
        spec.cap_add.is_empty() && spec.cap_drop.is_empty(),
        "caps ⇒ empty"
    );
    assert!(
        !spec.privileged && !spec.tty && !spec.init && !spec.read_only,
        "bools ⇒ false"
    );
    assert!(spec.oom_score_adj.is_none(), "oom_score_adj ⇒ None");
    assert!(spec.pids_limit.is_none(), "pids_limit ⇒ None");
    assert!(spec.shm_size.is_none(), "shm_size ⇒ None");
    assert!(
        spec.hostname.is_none() && spec.labels.is_empty(),
        "hostname/labels ⇒ default"
    );
}

/// The new RC carry-fields write/read round-trip through spec.json unchanged —
/// what the detached supervisor reads back to drive the apply seam.
#[test]
fn spec_on_disk_rc_seam_fields_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SpecOnDisk {
        cwd: "/w".to_string(),
        command: vec!["sh".to_string()],
        detached: true,
        created_at_unix: 1,
        hostname: Some("host-a".to_string()),
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
    write_spec_json(dir.path(), &spec).expect("write");
    let back = read_spec_on_disk(dir.path()).expect("read");
    assert_eq!(back.hostname.as_deref(), Some("host-a"));
    assert_eq!(back.labels, vec![("k".to_string(), "v".to_string())]);
    assert_eq!(back.cap_add, vec!["NET_ADMIN".to_string()]);
    assert_eq!(back.cap_drop, vec!["MKNOD".to_string()]);
    assert!(back.privileged && back.tty && back.init && back.read_only);
    assert_eq!(back.oom_score_adj, Some(-500));
    assert_eq!(back.pids_limit, Some(64));
    assert_eq!(back.shm_size, Some(67_108_864));
}

// ── WP-RESLIMITS: resource caps thread RunSpec → SpecOnDisk → spec.json ─────

/// A spec.json written before the limits fields existed still parses, with both
/// caps at their no-op default (`None` ⇒ unlimited) — back-compat (behavior-
/// preserving for any pre-WP-RESLIMITS detached run dir).
#[test]
fn spec_on_disk_legacy_json_defaults_limits_to_unlimited() {
    let legacy = r#"{
        "cwd": "/w", "command": ["sleep","1"], "env_keys": [],
        "mounts": [], "detached": true, "created_at_unix": 1
    }"#;
    let spec: SpecOnDisk = serde_json::from_str(legacy).expect("legacy spec parses");
    assert!(spec.mem_limit_bytes.is_none(), "missing memory ⇒ unlimited");
    assert!(spec.cpu_limit_millis.is_none(), "missing cpus ⇒ unlimited");
}

/// The resource caps write/read round-trip through spec.json unchanged — what the
/// detached supervisor reads back to size + cap the child (RLIMIT_AS on Linux;
/// cpu recorded). This is the core #57 plumbing: limits SURVIVE serialization.
#[test]
fn spec_on_disk_limits_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SpecOnDisk {
        cwd: "/w".to_string(),
        command: vec!["sh".to_string()],
        detached: true,
        created_at_unix: 1,
        mem_limit_bytes: Some(512 * 1024 * 1024),
        cpu_limit_millis: Some(1500),
        ..Default::default()
    };
    write_spec_json(dir.path(), &spec).expect("write");
    let back = read_spec_on_disk(dir.path()).expect("read");
    assert_eq!(back.mem_limit_bytes, Some(512 * 1024 * 1024));
    assert_eq!(back.cpu_limit_millis, Some(1500));
}

/// `RunSpec.limits` (the runtime type) threads onto `SpecOnDisk`'s flat fields at
/// the `spawn_detached_engine` serialize step. Asserted directly on the on-disk
/// shape so it is parallel-safe (no spawn, no env): the supervisor reconstructs a
/// `ResourceLimits` from exactly these two fields. `None`/`None` ⇒ unlimited.
#[test]
fn runspec_limits_map_onto_spec_on_disk_fields() {
    use crate::run::types::RunSpec;
    // Unlimited ⇒ both on-disk fields None (behavior-preserving default).
    let none = RunSpec::default();
    assert!(none.limits.is_unlimited());

    // A set cap maps field-for-field (the mapping `spawn.rs` performs).
    let set = RunSpec {
        limits: lightr_core::ResourceLimits {
            memory_bytes: Some(256 * 1024 * 1024),
            cpu_millis: Some(500),
        },
        ..Default::default()
    };
    assert_eq!(set.limits.memory_bytes, Some(256 * 1024 * 1024));
    assert_eq!(set.limits.cpu_millis, Some(500));
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

// ── WP-RC-USER: apply_user parse + cfg-gated honor ─────────────────────────

/// `apply_user(None)` is a NO-OP (Ok) on every platform — the no-`-u` path runs
/// as the current user, byte-identical to before (behavior-preserving).
#[test]
fn apply_user_none_is_noop() {
    let mut cmd = std::process::Command::new("true");
    crate::run::spawn::apply_user(&mut cmd, None).expect("None must be a no-op Ok");
}

/// A non-numeric `--user` is an HONEST parse error on EVERY platform (the parse
/// runs before any cfg branch): name resolution needs the container's
/// /etc/passwd, so only numeric `uid[:gid]` is the faithful native path.
#[test]
fn apply_user_nonnumeric_name_errors() {
    let mut cmd = std::process::Command::new("true");
    assert!(
        crate::run::spawn::apply_user(&mut cmd, Some("alice")).is_err(),
        "a non-numeric user name must be an honest error (no /etc/passwd)"
    );
    let mut cmd2 = std::process::Command::new("true");
    assert!(
        crate::run::spawn::apply_user(&mut cmd2, Some("1000:devs")).is_err(),
        "a non-numeric group must be an honest error"
    );
    let mut cmd3 = std::process::Command::new("true");
    assert!(
        crate::run::spawn::apply_user(&mut cmd3, Some("")).is_err(),
        "an empty --user value must be an honest error"
    );
}

/// On unix, numeric `uid[:gid]` parses + applies to the Command (Ok). We do NOT
/// spawn — applying uid/gid to the builder needs no privilege; the EPERM for a
/// non-root uid change surfaces at exec, not here. cfg(unix) so the windows
/// clippy gate (where this is an honest Err) never sees these bindings.
#[cfg(unix)]
#[test]
fn apply_user_numeric_ok_on_unix() {
    let mut cmd = std::process::Command::new("true");
    crate::run::spawn::apply_user(&mut cmd, Some("1000")).expect("numeric uid parses+applies");
    let mut cmd2 = std::process::Command::new("true");
    crate::run::spawn::apply_user(&mut cmd2, Some("1000:1000"))
        .expect("numeric uid:gid parses+applies");
}

/// On windows, a POSIX `--user` (even numeric) is an HONEST error — uid/gid has
/// no meaning. cfg(not(unix)) so the unix gate never sees this binding.
#[cfg(not(unix))]
#[test]
fn apply_user_unsupported_on_windows() {
    let mut cmd = std::process::Command::new("cmd");
    assert!(
        crate::run::spawn::apply_user(&mut cmd, Some("1000:1000")).is_err(),
        "POSIX uid/gid is unsupported on windows — honest error"
    );
}
