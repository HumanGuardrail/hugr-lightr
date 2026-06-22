//! ADD tar auto-extract (WP-DF-07, completed by WP-G): archive classification +
//! extraction.
//!
//! Split from `exec_fs.rs` (godfile cap) and scoped to the ADD-specific feature.
//! ALL four Docker-recognized local tar archives extract in-process via a `Read`
//! decoder feeding `tar::Archive`, one decoder per compression family:
//!   `.tar`            -> raw file              (no decoder)
//!   `.tar.gz`/`.tgz`  -> `flate2::read::GzDecoder`
//!   `.tar.bz2`/`.tbz2`-> `bzip2::read::BzDecoder`   (WP-G)
//!   `.tar.xz`/`.txz`  -> `xz2::read::XzDecoder`     (WP-G)
//! Every family fails CLOSED on a malformed stream (the decoder surfaces an IO
//! error through `unpack`), never a silent copy.
// Declared as a `#[path]` submodule of `exec_fs` (see exec_fs.rs), so `super`
// is the `exec_fs` module — `CopyMeta`/`apply_meta` live there.
use super::{apply_meta, CopyMeta};
use lightr_core::{LightrError, Result};
use std::path::Path;

/// Recognized LOCAL archive kinds for ADD's auto-extract (Docker semantics:
/// ADD of a local archive EXTRACTS it into dest rather than copying the file).
///
/// All four kinds are handled in-process (WP-G wired bzip2/xz decoders); the
/// classification is purely suffix-driven and shared with the memo key path.
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
/// Each kind picks its decompressor and unpacks through the shared `unpack_tar`:
/// `.tar` raw, `.tar.gz` via `flate2`, `.tar.bz2` via `bzip2`, `.tar.xz` via
/// `xz2` (WP-G). A corrupt stream fails closed through the decoder's IO error.
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
        ArchiveKind::TarBz2 => {
            let bz = bzip2::read::BzDecoder::new(file);
            unpack_tar(tar::Archive::new(bz), dest)?;
        }
        ArchiveKind::TarXz => {
            let xz = xz2::read::XzDecoder::new(file);
            unpack_tar(tar::Archive::new(xz), dest)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a single-entry tar (`hello.txt` = `payload`) into a `Vec<u8>`.
    fn make_tar(payload: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "hello.txt", payload)
            .unwrap();
        builder.into_inner().unwrap()
    }

    /// Write `bytes` to `dir/<name>` and return the path (a fixture archive).
    fn write_fixture(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// Round-trip: classify by suffix, extract, assert `hello.txt` content.
    fn assert_extracts(name: &str, archive_bytes: &[u8], expect: &[u8]) {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_fixture(tmp.path(), name, archive_bytes);
        let kind = archive_kind(&src).unwrap_or_else(|| panic!("{name} not classified"));
        let dest = tmp.path().join("out");
        extract_archive(&src, &dest, kind, &CopyMeta::default()).unwrap();
        let got = std::fs::read(dest.join("hello.txt")).unwrap();
        assert_eq!(got, expect, "extracted content mismatch for {name}");
    }

    #[test]
    fn tar_plain_extracts() {
        let tar = make_tar(b"plain-payload");
        assert_extracts("a.tar", &tar, b"plain-payload");
    }

    #[test]
    fn tar_gz_extracts() {
        let tar = make_tar(b"gz-payload");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&tar).unwrap();
        let gz = enc.finish().unwrap();
        assert_extracts("a.tar.gz", &gz, b"gz-payload");
    }

    #[test]
    fn tar_bz2_extracts() {
        let tar = make_tar(b"bz2-payload");
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        enc.write_all(&tar).unwrap();
        let bz = enc.finish().unwrap();
        assert_extracts("a.tar.bz2", &bz, b"bz2-payload");
    }

    #[test]
    fn tar_xz_extracts() {
        let tar = make_tar(b"xz-payload");
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(&tar).unwrap();
        let xz = enc.finish().unwrap();
        assert_extracts("a.tar.xz", &xz, b"xz-payload");
    }

    #[test]
    fn corrupt_compressed_stream_fails_closed() {
        // Bytes that are NOT a valid xz stream must error, never silently no-op.
        let tmp = tempfile::tempdir().unwrap();
        let src = write_fixture(tmp.path(), "bad.tar.xz", b"not-an-xz-stream");
        let kind = archive_kind(&src).unwrap();
        let dest = tmp.path().join("out");
        assert!(extract_archive(&src, &dest, kind, &CopyMeta::default()).is_err());
    }
}
