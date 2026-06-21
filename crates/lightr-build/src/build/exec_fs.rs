//! Build filesystem helpers: materialize_from_digest (CAS → work dir),
//! copy_dir_recursive (COPY/ADD), step_reads_clock_or_net (--explain heuristic).
//! Split from `build/exec.rs` (behavior-preserving) to keep both files <400 LOC.
use lightr_core::{Digest, LightrError, Manifest, Result};
use lightr_store::Store;
use std::path::Path;

// ADD tar auto-extract (WP-DF-07) lives in a sibling file, declared as a
// `#[path]` submodule of `exec_fs` (godfile cap). `place_*` below call into it;
// `ArchiveKind` stays reachable as `tar_extract::ArchiveKind` if ever needed.
#[path = "exec_fs_tar.rs"]
pub(crate) mod tar_extract;
use tar_extract::{archive_kind, extract_archive};

use crate::build::dockerignore::DockerIgnore;

/// WP-DF-IGNORE: the `.dockerignore` filter threaded through context COPY/ADD
/// placement — `(context_root, matcher)`. `Some` ⇒ a source path whose
/// context-relative path is excluded is NOT placed (recursively). `None` ⇒ no
/// filtering (no `.dockerignore`, or `COPY --from=stage` where a prior-stage
/// tree is not the build context). A helper keeps the per-entry check terse.
pub(crate) type IgnoreFilter<'a> = Option<(&'a Path, &'a DockerIgnore)>;

/// `true` if `path` (an absolute path under `context_root`) is excluded by the
/// filter. `None` filter ⇒ never excluded (byte-identical to no filter). A path
/// not under `context_root` (defensive) is kept.
fn filtered_out(path: &Path, filter: IgnoreFilter) -> bool {
    match filter {
        Some((root, ignore)) => match path.strip_prefix(root) {
            Ok(rel) => {
                let rel = rel.to_string_lossy();
                !rel.is_empty() && ignore.is_excluded(&rel)
            }
            Err(_) => false,
        },
        None => false,
    }
}

/// Materialize a snapshot (identified by its manifest digest) into `dest`.
/// Clears `dest` first so we get a clean layer.
pub(crate) fn materialize_from_digest(
    dest: &Path,
    manifest_digest: &Digest,
    store: &Store,
) -> Result<()> {
    if dest.exists() {
        for entry in std::fs::read_dir(dest).map_err(LightrError::Io)? {
            let entry = entry.map_err(LightrError::Io)?;
            let p = entry.path();
            if p.is_dir() && !p.is_symlink() {
                std::fs::remove_dir_all(&p).map_err(LightrError::Io)?;
            } else {
                std::fs::remove_file(&p).map_err(LightrError::Io)?;
            }
        }
    } else {
        std::fs::create_dir_all(dest).map_err(LightrError::Io)?;
    }

    let manifest_bytes = store.get_bytes(manifest_digest)?;
    let manifest = Manifest::decode(&manifest_bytes)?;

    for entry in &manifest.entries {
        match entry {
            lightr_core::Entry::Dir { path } => {
                std::fs::create_dir_all(dest.join(path)).map_err(LightrError::Io)?;
            }
            lightr_core::Entry::File {
                path, mode, digest, ..
            } => {
                if let Some(parent) = Path::new(path).parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(dest.join(parent)).map_err(LightrError::Io)?;
                    }
                }
                store.materialize_file(digest, &dest.join(path), *mode)?;
            }
            lightr_core::Entry::Symlink { path, target } => {
                if let Some(parent) = Path::new(path).parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(dest.join(parent)).map_err(LightrError::Io)?;
                    }
                }
                let link_path = dest.join(path);
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &link_path).map_err(LightrError::Io)?;
                #[cfg(windows)]
                {
                    use std::os::windows::fs::symlink_file;
                    if symlink_file(target, &link_path).is_err() {
                        let resolved_target = if std::path::Path::new(target).is_absolute() {
                            std::path::PathBuf::from(target)
                        } else {
                            link_path
                                .parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .join(target)
                        };
                        if resolved_target.exists() {
                            std::fs::copy(&resolved_target, &link_path).map_err(LightrError::Io)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Recursively copy `src`'s contents into `dest`, applying COPY's
/// `--chown`/`--chmod` (`meta`) to every copied file AND directory (Docker
/// applies the flags recursively). A `CopyMeta::default()` (no flags) is
/// byte-identical to a plain recursive copy (chmod/chown become no-ops).
pub(crate) fn copy_dir_recursive_meta(
    src: &Path,
    dest: &Path,
    meta: &CopyMeta,
    filter: IgnoreFilter,
) -> Result<()> {
    for entry in std::fs::read_dir(src).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        // WP-DF-IGNORE: skip a child whose context-relative path is excluded —
        // so `COPY . /dst` (or a copied subdir) never materializes ignored files.
        // A `None` filter makes this a no-op (byte-identical recursive copy).
        if filtered_out(&entry.path(), filter) {
            continue;
        }
        let ft = entry.file_type().map_err(LightrError::Io)?;
        let target = dest.join(entry.file_name());
        if ft.is_dir() {
            std::fs::create_dir_all(&target).map_err(LightrError::Io)?;
            copy_dir_recursive_meta(&entry.path(), &target, meta, filter)?;
            apply_meta(&target, meta)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &target).map_err(LightrError::Io)?;
            apply_meta(&target, meta)?;
        }
    }
    Ok(())
}

/// Resolved COPY `--chown`/`--chmod` flags, applied to copied entries.
///
/// `mode` is the parsed octal `--chmod` (e.g. `0o644`). `uid`/`gid` are the
/// NUMERIC ids from `--chown=uid:gid`. Named users/groups are best-effort:
/// without resolving the target image's `/etc/passwd` we cannot map a name to an
/// id honestly, so a non-numeric component is left `None` (the entry keeps the
/// copying process's ownership) rather than guessing — an honest no-op, recorded
/// here so callers and tests can reason about it.
#[derive(Clone, Copy, Default)]
pub(crate) struct CopyMeta {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl CopyMeta {
    /// Parse the `--chmod` (octal) + `--chown` (`user[:group]`) flag strings.
    /// `--chmod` must be valid octal (fail-closed on garbage). `--chown` numeric
    /// components are honored; named components are dropped (best-effort, honest).
    pub(crate) fn parse(chown: Option<&str>, chmod: Option<&str>) -> Result<Self> {
        let mode = match chmod {
            Some(s) => Some(u32::from_str_radix(s.trim(), 8).map_err(|_| {
                LightrError::InvalidManifest(format!("COPY --chmod: invalid octal mode {s:?}"))
            })?),
            None => None,
        };
        let (mut uid, mut gid) = (None, None);
        if let Some(s) = chown {
            let (u, g) = match s.split_once(':') {
                Some((u, g)) => (u, Some(g)),
                None => (s, None),
            };
            uid = u.trim().parse::<u32>().ok();
            gid = g.and_then(|g| g.trim().parse::<u32>().ok());
        }
        Ok(CopyMeta { mode, uid, gid })
    }
}

/// Apply a `CopyMeta`'s chmod/chown to a single path (no-op for empty meta and
/// on non-unix, where the POSIX mode/owner model does not apply — honest skip).
pub(crate) fn apply_meta(path: &Path, meta: &CopyMeta) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = meta.mode {
            let perm = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(path, perm).map_err(LightrError::Io)?;
        }
        if meta.uid.is_some() || meta.gid.is_some() {
            let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| LightrError::InvalidManifest("COPY: path has NUL byte".into()))?;
            // chown(uid=-1)/gid=-1 leaves that id unchanged (POSIX), so a
            // single-sided --chown (or an unresolved name) is an honest no-op.
            let uid = meta.uid.unwrap_or(u32::MAX) as libc::uid_t;
            let gid = meta.gid.unwrap_or(u32::MAX) as libc::gid_t;
            let rc = unsafe { libc::chown(c.as_ptr(), uid, gid) };
            if rc != 0 {
                // Non-root cannot chown to arbitrary ids; surface this honest,
                // expected failure rather than silently dropping the ownership.
                return Err(LightrError::Io(std::io::Error::last_os_error()));
            }
        }
    }
    #[cfg(not(unix))]
    {
        // POSIX mode/owner has no meaning here; read every field so the windows
        // build (build+clippy gate) sees no dead struct field — honest no-op.
        let _ = (path, meta.mode, meta.uid, meta.gid);
    }
    Ok(())
}

/// Expand one COPY source token against `context_dir`, honoring `*`/`?` globs in
/// the FINAL path component (Docker's COPY glob surface). A token with no glob
/// metachar returns the single literal path (even if missing — the caller's
/// copy/key logic already handles a missing source faithfully). A glob with no
/// matches yields an empty vec (the caller errors, matching Docker's "no source
/// files were specified").
pub(crate) fn expand_glob(context_dir: &Path, token: &str) -> Vec<std::path::PathBuf> {
    if !token.contains('*') && !token.contains('?') {
        return vec![context_dir.join(token)];
    }
    let rel = Path::new(token);
    let (parent, pat) = match (rel.parent(), rel.file_name()) {
        (Some(p), Some(f)) => (p.to_path_buf(), f.to_string_lossy().into_owned()),
        _ => return vec![context_dir.join(token)],
    };
    let dir = context_dir.join(&parent);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            if glob_match(&pat, &name.to_string_lossy()) {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

/// Minimal shell-style glob match for `*` (any run) and `?` (one char) on a
/// single path component — Docker's COPY uses Go `filepath.Match` semantics; we
/// implement the two metachars the common `COPY *.txt`/`COPY file?.c` forms use.
/// Dotfiles are matched (Docker's COPY glob does not special-case a leading dot).
pub(crate) fn glob_match(pat: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pat.chars().collect(), name.chars().collect());
    // Classic two-pointer wildcard match with backtracking on `*`.
    let (mut pi, mut ni, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Place resolved `sources` at `dest` (under `work_dir`) by the COPY/ADD
/// directory rules, shared by both instructions (DF-07 reuses DF-06's placement).
/// When `extract` (ADD), a source that is a recognized archive FILE is
/// auto-extracted into the dir-dest; COPY passes `false`. `dest` is a DIRECTORY
/// when it ends in `/`, there is >1 source, or (ADD only) any source auto-extracts.
pub(crate) fn place_sources(
    work_dir: &Path,
    sources: &[std::path::PathBuf],
    dest: &str,
    meta: &CopyMeta,
    extract: bool,
    filter: IgnoreFilter,
) -> Result<()> {
    let dest_path = if dest.starts_with('/') {
        work_dir.join(dest.trim_start_matches('/'))
    } else {
        work_dir.join(dest)
    };
    let any_archive = extract
        && sources
            .iter()
            .any(|s| s.is_file() && archive_kind(s).is_some());
    let dest_is_dir = dest.ends_with('/') || sources.len() > 1 || any_archive;
    if dest_is_dir {
        std::fs::create_dir_all(&dest_path).map_err(LightrError::Io)?;
        for src_path in sources {
            place_one_into_dir(src_path, &dest_path, meta, extract, filter)?;
        }
    } else {
        // Single non-archive source, file-or-dir dest (COPY semantics exactly).
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
        }
        let src_path = &sources[0];
        if src_path.is_file() {
            std::fs::copy(src_path, &dest_path).map_err(LightrError::Io)?;
            apply_meta(&dest_path, meta)?;
        } else if src_path.is_dir() {
            std::fs::create_dir_all(&dest_path).map_err(LightrError::Io)?;
            // WP-DF-IGNORE: nested excluded files under a copied dir are skipped.
            copy_dir_recursive_meta(src_path, &dest_path, meta, filter)?;
            apply_meta(&dest_path, meta)?;
        }
    }
    Ok(())
}

/// Place one resolved source INTO `dest_dir`. When `extract` (ADD) and the source
/// is a recognized archive FILE, EXTRACT it (Docker auto-extract); otherwise a
/// file lands as `dest_dir/<name>` and a dir copies its CONTENTS into `dest_dir`.
fn place_one_into_dir(
    src_path: &Path,
    dest_dir: &Path,
    meta: &CopyMeta,
    extract: bool,
    filter: IgnoreFilter,
) -> Result<()> {
    if extract && src_path.is_file() {
        if let Some(kind) = archive_kind(src_path) {
            return extract_archive(src_path, dest_dir, kind, meta);
        }
    }
    if src_path.is_file() {
        let file_name = src_path.file_name().ok_or_else(|| {
            LightrError::InvalidManifest("COPY/ADD: source has no file name".into())
        })?;
        let target = dest_dir.join(file_name);
        std::fs::copy(src_path, &target).map_err(LightrError::Io)?;
        apply_meta(&target, meta)?;
    } else if src_path.is_dir() {
        // WP-DF-IGNORE: a dir source copies its CONTENTS; excluded nested files
        // are skipped by the recursive copy (a `None` filter is unchanged).
        copy_dir_recursive_meta(src_path, dest_dir, meta, filter)?;
    }
    Ok(())
}

/// Heuristic: does this argv likely read the clock or network?
/// Used by `--explain` in the CLI (W3) to flag non-reproducible RUN steps.
pub fn step_reads_clock_or_net(argv: &[String]) -> bool {
    let cmd = argv.join(" ");
    let patterns = [
        "date",
        "curl",
        "wget",
        "fetch",
        "apt-get",
        "apk",
        "yum",
        "pip",
        "npm",
        "cargo install",
    ];
    patterns.iter().any(|p| cmd.contains(p))
}
