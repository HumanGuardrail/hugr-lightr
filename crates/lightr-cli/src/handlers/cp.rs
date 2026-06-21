//! `lightr cp` handler — copy files/dirs between a container and the host
//! (`docker cp`). WP-CP-REAL: replaces the CLI-surface-freeze stub.
//!
//! Forms (exactly ONE side carries the `<container>:` prefix):
//!   - `lightr cp <container>:<src> <host_dest>`   container → host
//!   - `lightr cp <host_src> <container>:<dest>`   host → container
//!
//! Both prefixed or neither prefixed ⇒ usage error (exit 2), matching
//! `docker cp`'s "must specify at least one container source" / "copying
//! between containers is not supported".
//!
//! A "container" here is a detached run: its materialized filesystem root is
//! `<home>/run/<id>/rootfs` (created on spawn — see `lightr_run::run::svz`).
//! The user-supplied ref is resolved with `lightr_run::resolve`; a miss routes
//! through `die_resolve` ⇒ "No such container" + exit 1 (Docker parity).
//!
//! Copy rules transcribe the common `docker cp` cases: file→file, file→dir(/),
//! dir→dir (recursive). Path traversal is fail-closed: a container path that
//! escapes the rootfs via `..` is rejected.
//!
//! Ambiguity notes (minimal Docker-faithful choices, per WP brief):
//!  - We resolve the container's filesystem to `<run>/rootfs`. A `native` run
//!    has no separate rootfs (it executes against the host), so `rootfs` may be
//!    absent; we treat an absent rootfs as an empty container root — a missing
//!    container *path* then surfaces the honest "no such file" error (exit 1),
//!    never a silent success.
//!  - Symlinks inside the container are copied as-is (not followed across the
//!    rootfs boundary); a symlink whose link path contains `..` cannot escape
//!    because we never *follow* it during traversal — we copy the link target
//!    string verbatim only on unix, and copy the file's bytes otherwise.

use std::path::{Component, Path, PathBuf};

use lightr_core::LightrError;

use crate::{exit::die_resolve, lightr_home};

/// One side of a `cp` argument, after classifying the `<container>:` prefix.
enum Side {
    /// A path inside the named container.
    Container { reference: String, path: String },
    /// A plain host path.
    Host(PathBuf),
}

/// Classify a `cp` argument as a container path or a host path, transcribing
/// Docker's `splitCpArg`: the arg is `container:path` iff it contains a `:` and
/// the segment BEFORE the first `:` is non-empty and contains no path separator
/// (so a host path like `./a:b` or `/abs:x` stays a host path). A leading-`:`
/// arg (empty container) is a host path too.
fn classify(arg: &str) -> Side {
    if let Some(idx) = arg.find(':') {
        let (head, tail) = (&arg[..idx], &arg[idx + 1..]);
        let looks_like_container = !head.is_empty() && !head.contains('/') && !head.contains('\\');
        if looks_like_container {
            return Side::Container {
                reference: head.to_string(),
                path: tail.to_string(),
            };
        }
    }
    Side::Host(PathBuf::from(arg))
}

/// Resolve `<container>:<path>` to an absolute host path inside the container's
/// rootfs, fail-closed against `..` traversal that escapes the rootfs.
///
/// Returns the exit code on failure (already printed), or the joined host path.
fn resolve_container_path(home: &Path, reference: &str, path: &str) -> Result<PathBuf, i32> {
    let id = match lightr_run::resolve(home, reference) {
        Ok(id) => id,
        Err(e) => return Err(die_resolve(&e, reference)),
    };
    let root = home.join("run").join(&id).join("rootfs");
    join_under_root(&root, path).map_err(|e| {
        eprintln!("lightr: {e}");
        1
    })
}

/// Join a container-internal `path` onto `root`, rejecting any normalized
/// component that would escape `root` (fail-closed `..` guard). The container
/// path is treated as absolute-from-rootfs (a leading `/` is stripped); `.` is
/// dropped; `..` that would pop above `root` is an error.
fn join_under_root(root: &Path, path: &str) -> Result<PathBuf, LightrError> {
    let mut out = root.to_path_buf();
    let mut depth = 0usize;
    for comp in Path::new(path).components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => { /* anchor to root */ }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return Err(LightrError::InvalidRef(format!(
                        "path '{path}' escapes the container root"
                    )));
                }
                depth -= 1;
                out.pop();
            }
            Component::Normal(seg) => {
                depth += 1;
                out.push(seg);
            }
        }
    }
    Ok(out)
}

/// Whether the user wrote a trailing slash on the dest (Docker treats this as
/// "dest must be a directory").
fn has_trailing_slash(s: &str) -> bool {
    s.ends_with('/') || s.ends_with('\\')
}

pub fn run(src: &str, dest: &str) -> i32 {
    let home = lightr_home();

    match (classify(src), classify(dest)) {
        // Both prefixed ⇒ container→container, unsupported (Docker: exit 1, but
        // it's a usage-class arg error here) → exit 2 per the WP grammar.
        (Side::Container { .. }, Side::Container { .. }) => {
            eprintln!("lightr: copying between containers is not supported");
            2
        }
        // Neither prefixed ⇒ no container side ⇒ usage error.
        (Side::Host(_), Side::Host(_)) => {
            eprintln!(
                "lightr: \"cp\" requires exactly one of SRC or DEST to be \
                 a container path (CONTAINER:PATH)"
            );
            2
        }
        // container → host
        (Side::Container { reference, path }, Side::Host(dest_host)) => {
            let src_host = match resolve_container_path(&home, &reference, &path) {
                Ok(p) => p,
                Err(code) => return code,
            };
            copy_into(&src_host, &dest_host, has_trailing_slash(dest))
        }
        // host → container
        (Side::Host(src_host), Side::Container { reference, path }) => {
            let dest_host = match resolve_container_path(&home, &reference, &path) {
                Ok(p) => p,
                Err(code) => return code,
            };
            copy_into(&src_host, &dest_host, has_trailing_slash(dest))
        }
    }
}

/// Perform the copy of `src` (file or dir) to `dest`, applying Docker's
/// file/dir + trailing-slash rules. Returns the process exit code.
fn copy_into(src: &Path, dest: &Path, dest_trailing_slash: bool) -> i32 {
    let meta = match std::fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(_) => {
            eprintln!("lightr: no such file or directory: {}", src.display());
            return 1;
        }
    };

    if meta.is_dir() {
        // dir → dir. If dest exists and is a dir, copy src INTO it as
        // dest/<src_basename> (Docker). If dest does not exist, src's CONTENTS
        // become dest.
        let target = if dest.is_dir() {
            match src.file_name() {
                Some(name) => dest.join(name),
                None => dest.to_path_buf(),
            }
        } else {
            dest.to_path_buf()
        };
        match copy_dir_recursive(src, &target) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("lightr: {e}");
                1
            }
        }
    } else {
        // file → file | file → dir(/). A trailing slash on a non-dir dest is an
        // error (Docker: "not a directory").
        let target = if dest.is_dir() {
            match src.file_name() {
                Some(name) => dest.join(name),
                None => dest.to_path_buf(),
            }
        } else if dest_trailing_slash {
            eprintln!("lightr: destination {} is not a directory", dest.display());
            return 1;
        } else {
            dest.to_path_buf()
        };
        match copy_file_preserving(src, &target) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("lightr: {e}");
                1
            }
        }
    }
}

/// Copy a single file, creating parent dirs and preserving a reasonable mode.
fn copy_file_preserving(src: &Path, dest: &Path) -> Result<(), LightrError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
    }
    std::fs::copy(src, dest).map_err(LightrError::Io)?;
    Ok(())
}

/// Recursively copy a directory tree, preserving mode bits on unix.
fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), LightrError> {
    std::fs::create_dir_all(dest).map_err(LightrError::Io)?;
    #[cfg(unix)]
    copy_mode(src, dest)?;

    for entry in std::fs::read_dir(src).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let ty = entry.file_type().map_err(LightrError::Io)?;

        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_symlink() {
            copy_symlink(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(LightrError::Io)?;
            #[cfg(unix)]
            copy_mode(&from, &to)?;
        }
    }
    Ok(())
}

/// Recreate a symlink at `dest` pointing at the same target as `src`. On unix
/// we copy the link verbatim (never following it, so a `..` target cannot
/// escape during *our* traversal). On non-unix we fall back to copying the
/// resolved bytes.
#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<(), LightrError> {
    let target = std::fs::read_link(src).map_err(LightrError::Io)?;
    std::os::unix::fs::symlink(target, dest).map_err(LightrError::Io)?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, dest: &Path) -> Result<(), LightrError> {
    // No portable symlink primitive on the windows gate path; copy the bytes
    // the link resolves to (fail-closed: a broken link surfaces an I/O error).
    std::fs::copy(src, dest).map_err(LightrError::Io)?;
    Ok(())
}

/// Preserve the source file/dir's permission bits on unix.
#[cfg(unix)]
fn copy_mode(src: &Path, dest: &Path) -> Result<(), LightrError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(src)
        .map_err(LightrError::Io)?
        .permissions()
        .mode();
    std::fs::set_permissions(dest, std::fs::Permissions::from_mode(mode))
        .map_err(LightrError::Io)?;
    Ok(())
}

#[cfg(test)]
#[path = "cp_tests.rs"]
mod tests;
