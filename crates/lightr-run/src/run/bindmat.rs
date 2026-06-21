//! WP-RUNFLAGS — native materialization of Docker `-v/--volume` host binds and
//! `--tmpfs` dirs, plus the `--entrypoint` argv prepend.
//!
//! The native engine is a plain host process with NO mount namespace (CLAUDE.md
//! principle 4 — `native` = reproducibility, not a sandbox), so a Docker bind
//! mount is realized faithful-as-feasible by materializing the source at the
//! run's `cwd/<target>` (the same relative-target law `mounts` already uses):
//!
//!   * read-write bind  ⇒ a SYMLINK `cwd/<target>` → the host source. This is a
//!     LIVE view: a write inside the run hits the host path, exactly like a bind.
//!   * read-only bind   ⇒ a recursive SNAPSHOT COPY at `cwd/<target>` with every
//!     entry chmod'd read-only (cfg(unix)). Native has no mount-ns to enforce RO
//!     on a live symlink, so a read-only bind is an honest read-only snapshot of
//!     the host source at run start (noted in the WP card). The container sees the
//!     host content + cannot write it — both `:ro` acceptance facts hold.
//!
//! `--tmpfs <target>` ⇒ a fresh empty writable directory at `cwd/<target>`.
//!
//! All targets pass [`super::memo::validate_mount_target`] (relative, no `..`) so
//! a bind can never escape the run cwd — fail-closed, like the existing mounts.
//!
//! RUNTIME-ONLY: nothing here touches a memo key. The caller (memo/supervisor)
//! materializes AFTER the key is computed, and forces a MISS with no AC write
//! when any bind/tmpfs is present (a host-bound run is not reproducible).

use std::path::Path;

use lightr_core::{LightrError, Result};

use super::memo::validate_mount_target;
use super::types::VolumeBind;

/// Materialize every `-v/--volume` host bind into `cwd`. A read-write bind is a
/// live symlink; a read-only bind is a read-only snapshot copy. Empty ⇒ no-op
/// (behaviour-preserving). Fail-closed: a bad target / missing source errors.
pub(super) fn materialize_volumes(cwd: &Path, volumes: &[VolumeBind]) -> Result<()> {
    for v in volumes {
        validate_mount_target(&v.target)?;
        let dest = cwd.join(&v.target);
        let source = Path::new(&v.source);
        if !source.exists() {
            return Err(LightrError::InvalidRef(format!(
                "volume source does not exist: {}",
                v.source
            )));
        }
        // Replace any stale destination so a re-run is deterministic.
        remove_dest(&dest)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
        }
        if v.readonly {
            ro_snapshot(source, &dest)?;
        } else {
            symlink_bind(source, &dest)?;
        }
    }
    Ok(())
}

/// Materialize every `--tmpfs` target as a fresh empty writable directory under
/// `cwd`. Empty ⇒ no-op. Fail-closed on a bad target.
pub(super) fn materialize_tmpfs(cwd: &Path, tmpfs: &[String]) -> Result<()> {
    for target in tmpfs {
        validate_mount_target(target)?;
        let dest = cwd.join(target);
        // A tmpfs is empty scratch every run: clear any stale dest, recreate.
        remove_dest(&dest)?;
        std::fs::create_dir_all(&dest).map_err(LightrError::Io)?;
    }
    Ok(())
}

/// Materialize the persisted, tagged `mounts2` host binds + tmpfs dirs (the
/// detached-supervisor path). Mirrors [`materialize_volumes`] + [`materialize_tmpfs`]
/// over the on-disk shape. CAS-ref / named / anon variants are NOT this WP's
/// scope (WP-VOL ring) — they are skipped here (left for a later WP), never
/// silently mis-materialized. Empty ⇒ no-op (behaviour-preserving).
pub(super) fn materialize_mounts2(
    cwd: &Path,
    mounts2: &[super::types::MountOnDisk2],
) -> Result<()> {
    use super::types::MountOnDisk2;
    for m in mounts2 {
        match m {
            MountOnDisk2::HostBind {
                source,
                target,
                readonly,
            } => {
                validate_mount_target(target)?;
                let dest = cwd.join(target);
                let src = Path::new(source);
                if !src.exists() {
                    return Err(LightrError::InvalidRef(format!(
                        "volume source does not exist: {source}"
                    )));
                }
                remove_dest(&dest)?;
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
                }
                if *readonly {
                    ro_snapshot(src, &dest)?;
                } else {
                    symlink_bind(src, &dest)?;
                }
            }
            MountOnDisk2::Tmpfs { target, .. } => {
                validate_mount_target(target)?;
                let dest = cwd.join(target);
                remove_dest(&dest)?;
                std::fs::create_dir_all(&dest).map_err(LightrError::Io)?;
            }
            // WP-VOL ring (out of WP-RUNFLAGS scope): CAS-ref / named / anon.
            MountOnDisk2::CasRef { .. }
            | MountOnDisk2::NamedVolume { .. }
            | MountOnDisk2::AnonVolume { .. } => {}
        }
    }
    Ok(())
}

/// Docker `--entrypoint`: the effective argv prepends the entrypoint to the CLI
/// `command` (which is Docker's CMD). `None` ⇒ `command` returned unchanged
/// (byte-identical to before). An empty `command` with an entrypoint runs just
/// the entrypoint.
pub(super) fn effective_argv(entrypoint: Option<&[String]>, command: &[String]) -> Vec<String> {
    match entrypoint {
        None => command.to_vec(),
        Some(ep) => {
            let mut argv = ep.to_vec();
            argv.extend_from_slice(command);
            argv
        }
    }
}

/// Remove an existing destination (file, dir, or symlink) so materialization is
/// idempotent. Absent ⇒ Ok. A symlink is removed without following it.
fn remove_dest(dest: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dest) {
        Ok(meta) => {
            if meta.file_type().is_dir() {
                std::fs::remove_dir_all(dest).map_err(LightrError::Io)
            } else {
                std::fs::remove_file(dest).map_err(LightrError::Io)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LightrError::Io(e)),
    }
}

/// Create a live symlink `dest` → `source` (the read-write bind). On unix uses
/// `std::os::unix::fs::symlink`; on windows the dir/file symlink variant.
fn symlink_bind(source: &Path, dest: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, dest).map_err(LightrError::Io)
    }
    #[cfg(windows)]
    {
        // WIN-PATH: a directory and a file need different symlink calls; pick by
        // the source kind. Symlink creation may need privilege/Developer Mode on
        // Windows — surfaced as an honest io error, never silently dropped.
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(source, dest).map_err(LightrError::Io)
        } else {
            std::os::windows::fs::symlink_file(source, dest).map_err(LightrError::Io)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (source, dest);
        Err(LightrError::InvalidRef(
            "volume binds are unsupported on this host".to_string(),
        ))
    }
}

/// Create a read-only snapshot copy of `source` at `dest` (the `:ro` bind on the
/// native engine, which has no mount namespace to enforce RO on a live symlink).
fn ro_snapshot(source: &Path, dest: &Path) -> Result<()> {
    copy_tree(source, dest)?;
    set_readonly_recursive(dest)?;
    Ok(())
}

/// Recursively copy `src` → `dst` (a file or a directory tree). Symlinks inside
/// the tree are copied as files (their target content) — a snapshot is content,
/// not links.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::metadata(src).map_err(LightrError::Io)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(LightrError::Io)?;
        for entry in std::fs::read_dir(src).map_err(LightrError::Io)? {
            let entry = entry.map_err(LightrError::Io)?;
            let name = entry.file_name();
            copy_tree(&src.join(&name), &dst.join(&name))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map_err(LightrError::Io)?;
        Ok(())
    }
}

/// Make every entry under `path` (the snapshot) read-only. cfg(unix): clears the
/// write bits (0o555 dirs need exec/list); cfg(other): the std `set_readonly`.
fn set_readonly_recursive(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).map_err(LightrError::Io)?;
    if meta.file_type().is_dir() {
        for entry in std::fs::read_dir(path).map_err(LightrError::Io)? {
            let entry = entry.map_err(LightrError::Io)?;
            set_readonly_recursive(&entry.path())?;
        }
    }
    set_readonly_one(path, meta.file_type().is_dir())
}

#[cfg(unix)]
fn set_readonly_one(path: &Path, is_dir: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // r-xr-xr-x for dirs (must keep exec/list), r--r--r-- for files.
    let mode = if is_dir { 0o555 } else { 0o444 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(LightrError::Io)
}

#[cfg(not(unix))]
fn set_readonly_one(path: &Path, _is_dir: bool) -> Result<()> {
    let mut perms = std::fs::metadata(path)
        .map_err(LightrError::Io)?
        .permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(path, perms).map_err(LightrError::Io)
}

#[cfg(test)]
#[path = "bindmat_tests.rs"]
mod tests;
