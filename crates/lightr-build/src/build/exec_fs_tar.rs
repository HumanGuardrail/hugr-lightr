//! ADD tar auto-extract (WP-DF-07): archive classification + extraction.
//!
//! Split from `exec_fs.rs` (godfile cap) and scoped to the ADD-specific feature.
//! `.tar`/`.tar.gz` are extracted in-process via the existing `tar`+`flate2`
//! workspace deps; `.tar.bz2`/`.tar.xz` are honestly deferred (no decompressor
//! dep, and WP-DF-07 forbids adding one) — fail-closed, never a silent copy.
// Declared as a `#[path]` submodule of `exec_fs` (see exec_fs.rs), so `super`
// is the `exec_fs` module — `CopyMeta`/`apply_meta` live there.
use super::{apply_meta, CopyMeta};
use lightr_core::{LightrError, Result};
use std::path::Path;

/// Recognized LOCAL archive kinds for ADD's auto-extract (Docker semantics:
/// ADD of a local archive EXTRACTS it into dest rather than copying the file).
///
/// `Tar` (uncompressed) and `TarGz` (gzip) are handled in-process. `TarBz2`/
/// `TarXz` are RECOGNIZED but honestly deferred (no bzip2/xz dep).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArchiveKind {
    Tar,
    TarGz,
    TarBz2,
    TarXz,
}

/// Classify a source path by Docker's recognized archive suffixes (case-
/// insensitive on the file name). A path that is not a recognized archive returns
/// `None` so ADD falls back to COPY semantics. (Docker only auto-extracts
/// archives that are local FILES; the caller pairs this with an `is_file()` check.)
pub(crate) fn archive_kind(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") {
        Some(ArchiveKind::TarBz2)
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        Some(ArchiveKind::TarXz)
    } else if name.ends_with(".tar") {
        Some(ArchiveKind::Tar)
    } else {
        None
    }
}

/// Extract a recognized LOCAL archive `src` INTO the directory `dest` (created if
/// absent) — Docker's ADD auto-extract. `--chmod`/`--chown` (`meta`) are applied
/// recursively to the extracted tree AFTER unpacking, mirroring COPY's per-entry
/// flag application — so an ADD with flags keys+behaves like the COPY equivalent.
///
/// `.tar` and `.tar.gz`/`.tgz` extract via `tar`(+`flate2`). `.tar.bz2`/`.tar.xz`
/// are HONESTLY DEFERRED — a fail-closed error, never a silent copy.
pub(crate) fn extract_archive(
    src: &Path,
    dest: &Path,
    kind: ArchiveKind,
    meta: &CopyMeta,
) -> Result<()> {
    std::fs::create_dir_all(dest).map_err(LightrError::Io)?;
    let file = std::fs::File::open(src).map_err(LightrError::Io)?;
    match kind {
        ArchiveKind::Tar => unpack_tar(tar::Archive::new(file), dest)?,
        ArchiveKind::TarGz => {
            let gz = flate2::read::GzDecoder::new(file);
            unpack_tar(tar::Archive::new(gz), dest)?;
        }
        ArchiveKind::TarBz2 | ArchiveKind::TarXz => {
            let fmt = if kind == ArchiveKind::TarBz2 {
                "bzip2 (.tar.bz2)"
            } else {
                "xz (.tar.xz)"
            };
            return Err(LightrError::InvalidManifest(format!(
                "ADD auto-extract of {fmt} is unsupported (no decompressor vendored); \
                 re-pack as .tar or .tar.gz, or COPY the archive and decompress in a RUN step"
            )));
        }
    }
    // Apply --chown/--chmod recursively to the extracted tree (mirrors COPY's
    // per-entry flag application). A flagless `CopyMeta::default()` is a no-op.
    apply_meta_recursive(dest, meta)
}

/// Unpack a `tar::Archive` into `dest`, fail-closed on any malformed entry. Path
/// traversal outside `dest` is rejected by `tar`'s own unpack guard; existing
/// entries are overwritten so a re-extracted layer is deterministic.
fn unpack_tar<R: std::io::Read>(mut archive: tar::Archive<R>, dest: &Path) -> Result<()> {
    archive.set_overwrite(true);
    archive.unpack(dest).map_err(LightrError::Io)
}

/// Apply `meta` (chmod/chown) to every file AND directory under `root`
/// recursively (Docker applies ADD's flags to the extracted tree). A
/// `CopyMeta::default()` short-circuits to a no-op so flagless ADD is unchanged.
fn apply_meta_recursive(root: &Path, meta: &CopyMeta) -> Result<()> {
    if meta.mode.is_none() && meta.uid.is_none() && meta.gid.is_none() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let ft = entry.file_type().map_err(LightrError::Io)?;
        let p = entry.path();
        if ft.is_dir() {
            apply_meta_recursive(&p, meta)?;
        }
        apply_meta(&p, meta)?;
    }
    Ok(())
}
