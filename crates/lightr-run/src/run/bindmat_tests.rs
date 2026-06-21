//! WP-RUNFLAGS — unit tests for the native bind/tmpfs/entrypoint materializers.
//! Parallel-safe: each test uses its own unique tempdir (atomic counter + nanos),
//! no process-global state.

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// A unique private tempdir, removed on drop. Unique under concurrent tests via
/// an atomic counter + nanos (house convention — see registry's TmpHome).
struct Tmp {
    dir: PathBuf,
}
impl Tmp {
    fn new(tag: &str) -> Self {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("lightr-bindmat-{tag}-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        Tmp { dir }
    }
    fn path(&self) -> &std::path::Path {
        &self.dir
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn entrypoint_none_is_command_unchanged() {
    let cmd = vec!["echo".to_string(), "hi".to_string()];
    assert_eq!(effective_argv(None, &cmd), cmd);
}

#[test]
fn entrypoint_prepends_to_command() {
    let ep = vec!["/bin/sh".to_string(), "-c".to_string()];
    let cmd = vec!["echo hi".to_string()];
    assert_eq!(
        effective_argv(Some(&ep), &cmd),
        vec!["/bin/sh", "-c", "echo hi"]
    );
}

#[test]
fn rw_volume_binds_host_file_visible_in_cwd() {
    let host = Tmp::new("rw-host");
    let cwd = Tmp::new("rw-cwd");
    // A host file the run should see at cwd/data/file.txt.
    std::fs::write(host.path().join("file.txt"), b"from-host").unwrap();

    let v = VolumeBind {
        source: host.path().to_string_lossy().into_owned(),
        target: "data".to_string(),
        readonly: false,
    };
    materialize_volumes(cwd.path(), std::slice::from_ref(&v)).unwrap();

    let seen = std::fs::read(cwd.path().join("data").join("file.txt")).unwrap();
    assert_eq!(seen, b"from-host", "rw bind must surface the host file");
}

#[test]
#[cfg(unix)]
fn rw_volume_is_live_writes_propagate_to_host() {
    let host = Tmp::new("rw-live-host");
    let cwd = Tmp::new("rw-live-cwd");
    let v = VolumeBind {
        source: host.path().to_string_lossy().into_owned(),
        target: "m".to_string(),
        readonly: false,
    };
    materialize_volumes(cwd.path(), std::slice::from_ref(&v)).unwrap();
    // A write through the bind lands on the host source (live symlink view).
    std::fs::write(cwd.path().join("m").join("w.txt"), b"x").unwrap();
    assert!(
        host.path().join("w.txt").exists(),
        "a rw bind must be a live view (write propagates to host)"
    );
}

#[test]
#[cfg(unix)]
fn ro_volume_is_read_only_and_visible() {
    let host = Tmp::new("ro-host");
    let cwd = Tmp::new("ro-cwd");
    std::fs::write(host.path().join("file.txt"), b"ro-content").unwrap();

    let v = VolumeBind {
        source: host.path().to_string_lossy().into_owned(),
        target: "ro".to_string(),
        readonly: true,
    };
    materialize_volumes(cwd.path(), std::slice::from_ref(&v)).unwrap();

    let dest = cwd.path().join("ro").join("file.txt");
    // Visible.
    assert_eq!(std::fs::read(&dest).unwrap(), b"ro-content");
    // Read-only: the mode has no write bits.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
    assert_eq!(mode & 0o222, 0, "ro snapshot must clear all write bits");
}

#[test]
fn tmpfs_is_empty_writable_dir() {
    let cwd = Tmp::new("tmpfs-cwd");
    materialize_tmpfs(cwd.path(), &["scratch".to_string()]).unwrap();
    let dest = cwd.path().join("scratch");
    assert!(dest.is_dir(), "tmpfs target must be a directory");
    assert_eq!(
        std::fs::read_dir(&dest).unwrap().count(),
        0,
        "tmpfs starts empty"
    );
    // Writable.
    std::fs::write(dest.join("t"), b"ok").unwrap();
}

#[test]
fn empty_inputs_are_noops() {
    let cwd = Tmp::new("noop-cwd");
    materialize_volumes(cwd.path(), &[]).unwrap();
    materialize_tmpfs(cwd.path(), &[]).unwrap();
    // The cwd is untouched (no new entries).
    assert_eq!(std::fs::read_dir(cwd.path()).unwrap().count(), 0);
}

#[test]
fn absolute_target_is_rejected() {
    let host = Tmp::new("abs-host");
    let cwd = Tmp::new("abs-cwd");
    let v = VolumeBind {
        source: host.path().to_string_lossy().into_owned(),
        target: "/etc/escape".to_string(),
        readonly: false,
    };
    assert!(
        materialize_volumes(cwd.path(), std::slice::from_ref(&v)).is_err(),
        "an absolute target must be rejected (no escape from cwd)"
    );
}

#[test]
fn parent_dir_target_is_rejected() {
    let cwd = Tmp::new("parent-cwd");
    assert!(
        materialize_tmpfs(cwd.path(), &["../escape".to_string()]).is_err(),
        "a `..` target must be rejected (no escape from cwd)"
    );
}

#[test]
fn missing_source_is_honest_error() {
    let cwd = Tmp::new("missing-cwd");
    let v = VolumeBind {
        source: "/no/such/host/path/lightr-xyz".to_string(),
        target: "m".to_string(),
        readonly: false,
    };
    let err = materialize_volumes(cwd.path(), std::slice::from_ref(&v)).unwrap_err();
    assert!(
        err.to_string().contains("source does not exist"),
        "missing source must be an honest error: {err}"
    );
}
