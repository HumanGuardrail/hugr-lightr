use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::common::lightr_cmd;

// ---------------------------------------------------------------------------
// Guard struct: stops a detached run on Drop so no process is leaked.
// ---------------------------------------------------------------------------
pub(super) struct RunGuard {
    pub(super) id: String,
    pub(super) home: PathBuf,
}

impl RunGuard {
    pub(super) fn new(id: &str, home: &Path) -> Self {
        RunGuard {
            id: id.to_owned(),
            home: home.to_path_buf(),
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // Best-effort stop; ignore errors during cleanup (may already be stopped).
        let _ = lightr_cmd(&self.home)
            .args(["stop", &self.id, "--grace", "1"])
            .output();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse `id=<id>` from stdout, returning the id string.
pub(super) fn parse_id_from_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("id=") {
            return rest.trim().to_owned();
        }
    }
    panic!("could not find 'id=<id>' in stdout:\n{text}");
}

/// Poll predicate up to `timeout`; sleep 100 ms between checks.
pub(super) fn poll_until<F>(timeout: Duration, mut pred: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Run `lightr ps --json` and return parsed JSON array.
pub(super) fn ps_json(home: &Path) -> serde_json::Value {
    let out = lightr_cmd(home)
        .args(["ps", "--json"])
        .output()
        .expect("ps --json must not fail to launch");
    assert_eq!(out.status.code().unwrap_or(-1), 0, "ps --json must exit 0");
    serde_json::from_slice(&out.stdout).expect("ps --json must produce valid JSON")
}

/// Return true when the given id has running==true in `ps --json`.
pub(super) fn ps_is_running(home: &Path, id: &str) -> bool {
    let arr = ps_json(home);
    let Some(arr) = arr.as_array() else {
        return false;
    };
    for item in arr {
        if item.get("id").and_then(|v| v.as_str()) == Some(id) {
            return item
                .get("running")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        }
    }
    false
}

/// Return true when the given id is present in `ps --json` and running==false.
pub(super) fn ps_is_exited(home: &Path, id: &str) -> bool {
    let arr = ps_json(home);
    let Some(arr) = arr.as_array() else {
        return false;
    };
    for item in arr {
        if item.get("id").and_then(|v| v.as_str()) == Some(id) {
            return item
                .get("running")
                .and_then(|v| v.as_bool())
                .map(|r| !r)
                .unwrap_or(false);
        }
    }
    false
}

/// Recursively collect all regular file paths under `root`.
pub(super) fn collect_store_files(root: &Path) -> std::collections::HashSet<PathBuf> {
    let mut out = std::collections::HashSet::new();
    collect_store_files_recurse(root, &mut out);
    out
}

pub(super) fn collect_store_files_recurse(
    dir: &Path,
    out: &mut std::collections::HashSet<PathBuf>,
) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_file() {
            out.insert(path);
        } else if meta.file_type().is_dir() {
            collect_store_files_recurse(&path, out);
        }
    }
}

pub(super) fn count_files_under(root: &Path) -> usize {
    if !root.exists() {
        return 0;
    }
    let mut count = 0;
    count_files_recurse(root, &mut count);
    count
}

pub(super) fn count_files_recurse(dir: &Path, count: &mut usize) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_file() {
            *count += 1;
        } else if meta.file_type().is_dir() {
            count_files_recurse(&path, count);
        }
    }
}

// ---------------------------------------------------------------------------
// Shared tree comparison helper (reused from A11).
// ---------------------------------------------------------------------------
pub(super) fn compare_trees(expected: &Path, actual: &Path) {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    for entry in walkdir(expected) {
        let rel = entry.strip_prefix(expected).unwrap();
        let act = actual.join(rel);

        let exp_meta = fs::symlink_metadata(&entry).unwrap();

        if exp_meta.file_type().is_symlink() {
            let exp_target = fs::read_link(&entry).unwrap();
            let act_target = fs::read_link(&act)
                .unwrap_or_else(|_| panic!("missing symlink: {}", act.display()));
            assert_eq!(
                exp_target,
                act_target,
                "symlink target mismatch at {}",
                rel.display()
            );
        } else if exp_meta.file_type().is_dir() {
            assert!(act.is_dir(), "expected dir missing at {}", act.display());
            let exp_empty = fs::read_dir(&entry).unwrap().next().is_none();
            if exp_empty {
                let act_empty = fs::read_dir(&act).unwrap().next().is_none();
                assert!(
                    act_empty,
                    "expected empty dir but got contents at {}",
                    act.display()
                );
            }
        } else {
            let exp_bytes = fs::read(&entry).unwrap();
            let act_bytes =
                fs::read(&act).unwrap_or_else(|_| panic!("missing file: {}", act.display()));
            assert_eq!(
                exp_bytes,
                act_bytes,
                "file content mismatch at {}",
                rel.display()
            );
            #[cfg(unix)]
            {
                let exp_mode = exp_meta.permissions().mode() & 0o777;
                let act_meta = fs::metadata(&act).unwrap();
                let act_mode = act_meta.permissions().mode() & 0o777;
                assert_eq!(
                    exp_mode,
                    act_mode,
                    "file mode mismatch at {}: expected {:o} got {:o}",
                    rel.display(),
                    exp_mode,
                    act_mode
                );
            }
        }
    }
}

pub(super) fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walkdir_recurse(root, &mut out);
    out.sort();
    out
}

pub(super) fn walkdir_recurse(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(path.clone());
        if meta.file_type().is_dir() {
            walkdir_recurse(&path, out);
        }
    }
}

/// Recursively copy `src` into `dst` (dst must exist).
pub(super) fn copy_dir_all(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).unwrap().flatten() {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        let dest = dst.join(entry.file_name());
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &dest).unwrap();
            #[cfg(not(unix))]
            let _ = target; // symlinks not created on non-unix; dest is absent
        } else if meta.file_type().is_dir() {
            fs::create_dir_all(&dest).unwrap();
            copy_dir_all(&path, &dest);
        } else {
            fs::copy(&path, &dest).unwrap();
            let perms = meta.permissions();
            fs::set_permissions(&dest, perms).unwrap();
        }
    }
}
