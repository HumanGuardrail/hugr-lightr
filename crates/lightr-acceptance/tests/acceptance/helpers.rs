//! Non-`#[test]` helper fns shared across acceptance groups.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Walk `expected` recursively and assert `actual` matches byte-for-byte,
/// with identical st_mode & 0o777, symlink targets, and empty dirs.
pub(super) fn compare_trees(expected: &Path, actual: &Path) {
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
            // empty dir: check that it stays empty on both sides
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
            // regular file: bytes + mode
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

/// Sorted DFS walk of all entries under `root` (dirs included for empty-dir checks).
pub(super) fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    walk_recurse(root, &mut out);
    out.sort();
    out
}

pub(super) fn walk_recurse(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd {
        let entry = entry.unwrap();
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        out.push(path.clone());
        if meta.file_type().is_dir() {
            walk_recurse(&path, out);
        }
    }
}

#[cfg(unix)]
pub(super) fn assert_no_sockets_or_pidfiles(root: &Path) {
    use std::os::unix::fs::FileTypeExt;

    let entries = walkdir(root);
    for path in &entries {
        let meta = fs::symlink_metadata(path).unwrap();
        let ft = meta.file_type();
        assert!(
            !ft.is_socket(),
            "found unexpected socket in LIGHTR_HOME: {}",
            path.display()
        );
        assert!(
            !ft.is_fifo(),
            "found unexpected FIFO in LIGHTR_HOME: {}",
            path.display()
        );
        // pidfiles: no *.pid files
        if let Some(name) = path.file_name() {
            let name = name.to_string_lossy();
            assert!(
                !name.ends_with(".pid"),
                "found pidfile in LIGHTR_HOME: {}",
                path.display()
            );
        }
    }
}

/// Walk `objects_root` and return the path to one regular file (any).
pub(super) fn find_object_file(
    objects_root: &Path,
    pred: impl Fn(&[u8]) -> bool + Copy,
) -> Option<std::path::PathBuf> {
    fn recurse(dir: &Path, pred: &(impl Fn(&[u8]) -> bool + Copy)) -> Option<std::path::PathBuf> {
        let rd = fs::read_dir(dir).ok()?;
        for entry in rd {
            let entry = entry.ok()?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).ok()?;
            if meta.file_type().is_file() {
                if let Ok(bytes) = fs::read(&path) {
                    if !bytes.is_empty() && pred(&bytes) {
                        return Some(path);
                    }
                }
            } else if meta.file_type().is_dir() {
                if let Some(found) = recurse(&path, pred) {
                    return Some(found);
                }
            }
        }
        None
    }
    recurse(objects_root, &pred)
}

/// chmod writable, flip the first byte, reseal to 0o444 (spec: evidence kept).
pub(super) fn corrupt_in_place(object_file: &Path) {
    let mut content = fs::read(object_file).unwrap();
    assert!(!content.is_empty(), "object file must not be empty");
    #[cfg(unix)]
    fs::set_permissions(object_file, fs::Permissions::from_mode(0o644)).unwrap();
    content[0] ^= 0xFF;
    fs::write(object_file, &content).unwrap();
    #[cfg(unix)]
    fs::set_permissions(object_file, fs::Permissions::from_mode(0o444)).unwrap();
}
