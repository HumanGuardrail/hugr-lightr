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
