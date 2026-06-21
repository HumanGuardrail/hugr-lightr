//! Tests for `lightr cp` (WP-CP-REAL). Split out to keep `cp.rs` under the
//! 400-line godfile cap. PARALLEL-SAFE: every test uses its own tempdir home +
//! run id and exercises the injected-`home` helpers — no `set_var`, no global
//! state. The two grammar tests (`run`) hit the prefix-classification arms,
//! which return exit 2 BEFORE touching `lightr_home()`.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique, private container id (atomic counter + nanos) so concurrent tests
/// never collide on a run dir.
fn unique_id() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("cptest{n}x{nanos}")
}

/// Create a resolvable container under `home` with a `rootfs/` and return its id.
fn make_container(home: &Path) -> String {
    let id = unique_id();
    let rootfs = home.join("run").join(&id).join("rootfs");
    std::fs::create_dir_all(&rootfs).expect("mkdir rootfs");
    id
}

// ── grammar: prefix classification ──────────────────────────────────────────

#[test]
fn classify_distinguishes_container_from_host() {
    match classify("mycontainer:/etc/hosts") {
        Side::Container { reference, path } => {
            assert_eq!(reference, "mycontainer");
            assert_eq!(path, "/etc/hosts");
        }
        Side::Host(_) => panic!("expected a container side"),
    }
    // A host path with a colon after a slash is NOT a container ref.
    assert!(matches!(classify("./a:b"), Side::Host(_)));
    assert!(matches!(classify("/abs/path"), Side::Host(_)));
    // Leading colon ⇒ empty container ⇒ host path.
    assert!(matches!(classify(":/x"), Side::Host(_)));
}

#[test]
fn both_prefixed_is_usage_error_exit_2() {
    assert_eq!(run("a:/x", "b:/y"), 2);
}

#[test]
fn neither_prefixed_is_usage_error_exit_2() {
    assert_eq!(run("/tmp/a", "/tmp/b"), 2);
}

// ── traversal guard ─────────────────────────────────────────────────────────

#[test]
fn join_under_root_rejects_dotdot_escape() {
    let root = Path::new("/srv/run/abc/rootfs");
    assert!(join_under_root(root, "/../../etc/passwd").is_err());
    assert!(join_under_root(root, "../escape").is_err());
    // A `..` that stays within root is fine.
    let ok = join_under_root(root, "/a/b/../c").expect("stays inside");
    assert_eq!(ok, Path::new("/srv/run/abc/rootfs/a/c"));
    // Leading slash anchors to root, never to host /.
    let anchored = join_under_root(root, "/etc/hosts").expect("anchored");
    assert_eq!(anchored, Path::new("/srv/run/abc/rootfs/etc/hosts"));
}

#[test]
fn resolve_container_path_rejects_traversal() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let id = make_container(tmp.path());
    let code = resolve_container_path(tmp.path(), &id, "/../../../etc/passwd");
    assert!(code.is_err(), "traversal must be rejected");
    assert_eq!(code.unwrap_err(), 1);
}

// ── missing container ───────────────────────────────────────────────────────

#[test]
fn missing_container_resolves_to_exit_1() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    // No run dirs at all ⇒ resolve misses ⇒ die_resolve ⇒ exit 1.
    let code = resolve_container_path(tmp.path(), "ghost", "/etc/hosts");
    assert_eq!(code.unwrap_err(), 1, "missing container must exit 1");
}

// ── copy semantics ──────────────────────────────────────────────────────────

#[test]
fn file_to_host_file() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let id = make_container(tmp.path());
    let rootfs = tmp.path().join("run").join(&id).join("rootfs");
    std::fs::write(rootfs.join("greeting.txt"), b"hello").expect("write src");

    let src = resolve_container_path(tmp.path(), &id, "/greeting.txt").expect("src");
    let dest = tmp.path().join("out.txt");
    assert_eq!(copy_into(&src, &dest, false), 0);
    assert_eq!(std::fs::read(&dest).expect("read"), b"hello");
}

#[test]
fn host_file_to_container_dir() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let id = make_container(tmp.path());
    let rootfs = tmp.path().join("run").join(&id).join("rootfs");
    std::fs::create_dir_all(rootfs.join("dest")).expect("mkdir dest");

    let src = tmp.path().join("payload.bin");
    std::fs::write(&src, b"\x00\x01\x02").expect("write host src");

    let dest = resolve_container_path(tmp.path(), &id, "/dest").expect("dest");
    assert_eq!(copy_into(&src, &dest, false), 0);
    // file → existing dir copies in under the src basename.
    assert_eq!(
        std::fs::read(rootfs.join("dest").join("payload.bin")).expect("read"),
        b"\x00\x01\x02"
    );
}

#[test]
fn dir_to_dir_recursive() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let id = make_container(tmp.path());
    let rootfs = tmp.path().join("run").join(&id).join("rootfs");
    // Build a small tree inside the container: /data/{a.txt, sub/b.txt}.
    let data = rootfs.join("data");
    std::fs::create_dir_all(data.join("sub")).expect("mkdir tree");
    std::fs::write(data.join("a.txt"), b"A").expect("a");
    std::fs::write(data.join("sub").join("b.txt"), b"B").expect("b");

    let src = resolve_container_path(tmp.path(), &id, "/data").expect("src");
    // dest dir does NOT exist ⇒ src contents become dest.
    let dest = tmp.path().join("pulled");
    assert_eq!(copy_into(&src, &dest, false), 0);
    assert_eq!(std::fs::read(dest.join("a.txt")).expect("a"), b"A");
    assert_eq!(
        std::fs::read(dest.join("sub").join("b.txt")).expect("b"),
        b"B"
    );
}

#[test]
fn dir_to_existing_dir_nests_under_basename() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let src = tmp.path().join("mydir");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::write(src.join("f.txt"), b"x").expect("f");

    let dest = tmp.path().join("existing");
    std::fs::create_dir_all(&dest).expect("mkdir dest");

    assert_eq!(copy_into(&src, &dest, false), 0);
    // dest exists ⇒ src copied INTO it as dest/mydir/f.txt (Docker).
    assert_eq!(
        std::fs::read(dest.join("mydir").join("f.txt")).expect("read"),
        b"x"
    );
}

#[test]
fn missing_src_is_exit_1() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let src = tmp.path().join("does-not-exist");
    let dest = tmp.path().join("out");
    assert_eq!(copy_into(&src, &dest, false), 1);
}

#[test]
fn file_to_trailing_slash_nondir_is_error() {
    let tmp = tempfile::TempDir::new().expect("tmp");
    let src = tmp.path().join("f.txt");
    std::fs::write(&src, b"z").expect("write");
    // dest has a trailing slash but is not an existing dir ⇒ error (exit 1).
    let dest = tmp.path().join("nope");
    assert_eq!(copy_into(&src, &dest, true), 1);
}

#[cfg(unix)]
#[test]
fn file_mode_is_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::TempDir::new().expect("tmp");
    let src = tmp.path().join("script.sh");
    std::fs::write(&src, b"#!/bin/sh\n").expect("write");
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let destdir = tmp.path().join("d");
    std::fs::create_dir_all(&destdir).expect("mkdir");
    assert_eq!(copy_into(&src, &destdir, false), 0);

    let mode = std::fs::metadata(destdir.join("script.sh"))
        .expect("stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o750, "mode bits must be preserved");
}
