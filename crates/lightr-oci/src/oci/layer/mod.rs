//! Layer blob representation and apply_layers core.

mod apply;

use apply::{apply_ops, collect_ops};
use flate2::read::GzDecoder;
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::{
    fs,
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
};

// ─────────────────────────────────────────────────────────────────────────────
// Layer blob: in-memory bytes or a temp file path (for pull)
// ─────────────────────────────────────────────────────────────────────────────

pub(super) enum LayerBlob {
    /// The layer data lives at this path (owned by the caller's TempDirGuard).
    File(PathBuf),
    /// The layer data is a slice from a larger buffer (docker-save style).
    Bytes(Vec<u8>),
}

impl LayerBlob {
    /// Open a streaming `Read` over the layer, auto-detecting gzip by magic bytes.
    ///
    /// # Streaming design (no whole-layer Vec)
    ///
    /// For `File`: open → `BufReader` → read the first 2 bytes for the gzip magic
    /// (`0x1f 0x8b`). Those 2 bytes are chained back to the rest of the file via
    /// `io::Cursor::new([b0,b1]).chain(rest)` so the caller sees a complete stream.
    /// If gzip is detected the combined reader is wrapped in `flate2::read::GzDecoder`;
    /// otherwise it is returned as-is. At no point is the full file read into RAM.
    ///
    /// For `Bytes`: the same peek-and-chain logic is applied to an `io::Cursor` over
    /// the in-memory slice; behaviour is identical, no extra allocation.
    pub(super) fn open_reader(&self) -> io::Result<Box<dyn Read + '_>> {
        match self {
            LayerBlob::File(p) => {
                let file = fs::File::open(p)?;
                let mut reader = BufReader::new(file);
                // Peek the first 2 bytes to detect gzip magic.
                let mut magic = [0u8; 2];
                let n = reader.read(&mut magic)?;
                // Chain the consumed bytes back so the tarball sees a complete stream.
                let prefix = io::Cursor::new(magic[..n].to_vec());
                let full: Box<dyn Read> = Box::new(prefix.chain(reader));
                if n == 2 && magic[0] == 0x1f && magic[1] == 0x8b {
                    Ok(Box::new(GzDecoder::new(full)))
                } else {
                    Ok(full)
                }
            }
            LayerBlob::Bytes(b) => {
                let mut cursor = io::Cursor::new(b.as_slice());
                let mut magic = [0u8; 2];
                let n = cursor.read(&mut magic)?;
                let prefix = io::Cursor::new(magic[..n].to_vec());
                let full: Box<dyn Read> = Box::new(prefix.chain(cursor));
                if n == 2 && magic[0] == 0x1f && magic[1] == 0x8b {
                    Ok(Box::new(GzDecoder::new(full)))
                } else {
                    Ok(full)
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// layer_timeout_secs — per-call deadline config
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the per-call wall-clock deadline for `apply_layers`.
///
/// Default: 600 s.  Override via `LIGHTR_LAYER_TIMEOUT_SECS` (any non-integer
/// or value ≤ 0 silently falls back to the default).
pub(super) fn layer_timeout_secs() -> u64 {
    const DEFAULT_TIMEOUT_SECS: u64 = 600;
    std::env::var("LIGHTR_LAYER_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

// ─────────────────────────────────────────────────────────────────────────────
// apply_layers — private shared core (driver)
// ─────────────────────────────────────────────────────────────────────────────

/// Apply `blobs` in order into `tempdir`, honouring OCI whiteouts and path
/// safety. Returns the number of escaped entries that were skipped.
///
/// Each blob may be gzip-compressed (auto-detected by magic bytes 0x1f 0x8b)
/// or a plain tar archive.
///
/// # FIX 3 + 4: Intra-layer whiteout ordering
///
/// OCI spec: whiteout entries in a layer refer to the *parent* layer's
/// contents. Within a single layer we process ALL deletes (whiteouts) before
/// any additions so that a file added AND whited out in the same layer ends up
/// absent (OCI parent-ref semantics).
///
/// Implementation: two-pass per layer.
///   Pass 1 (`collect_ops`) — collect dirs to create, whiteouts to apply,
///             and pending file/symlink/hardlink writes.
///   Between passes — apply directory creates + all whiteouts.
///   Pass 2 (`apply_ops`) — write regular files and symlinks.
///   After pass 2 — resolve hardlinks (FIX 5).
pub(super) fn apply_layers(tempdir: &Path, blobs: &[LayerBlob]) -> Result<u64> {
    let timeout = layer_timeout_secs();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let mut skipped: u64 = 0;
    let mut entry_count: u64 = 0;

    for blob in blobs {
        // Open a streaming reader over the blob.
        //
        // `open_reader` peeks the first 2 bytes for gzip magic (0x1f 0x8b), chains
        // them back, and wraps in `flate2::read::GzDecoder` if compressed — all
        // without reading the full layer into a Vec.  The `tar` crate's `Archive`
        // accepts any `impl Read`, so decompression and entry parsing happen
        // chunk-by-chunk through a bounded I/O buffer.
        let reader = blob.open_reader().map_err(LightrError::Io)?;
        let mut archive = tar::Archive::new(reader);

        // ── Pass 1: collect all operations ───────────────────────────────────
        //
        // We parse the entire layer tar into three buckets:
        //   `dirs`      — directory entries (create first, before any writes)
        //   `whiteouts` — (parent_in_temp, whiteout_name or None for opaque)
        //   `pending`   — regular files, symlinks, hardlinks
        //
        // FIX 3: all whiteout operations execute before any file writes.
        // FIX 4: opaque whiteout clears the dir in the accumulated tree and
        //        creates it if absent.
        let (dirs, whiteouts, pending, whited_out_paths) = collect_ops(
            &mut archive,
            tempdir,
            deadline,
            &mut entry_count,
            &mut skipped,
            timeout,
        )?;

        // ── Pass 2: apply dirs → whiteouts → files → hardlinks ───────────────
        apply_ops(tempdir, &dirs, &whiteouts, &pending, &whited_out_paths)?;
    }

    Ok(skipped)
}

// ─────────────────────────────────────────────────────────────────────────────
// apply_and_snapshot — create a fresh tempdir, apply blobs, snapshot
// ─────────────────────────────────────────────────────────────────────────────

/// Create a fresh tempdir, apply the blobs, snapshot, return report.
pub(super) fn apply_and_snapshot(
    blobs: Vec<LayerBlob>,
    layer_count: u64,
    store: &Store,
    name: &str,
) -> Result<super::model::ImportReport> {
    use super::util::TempDirGuard;

    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tempdir = std::env::temp_dir().join(format!("lightr-oci-{pid}-{nanos}"));
    fs::create_dir_all(&tempdir).map_err(LightrError::Io)?;
    let _guard = TempDirGuard(tempdir.clone());

    let _skipped = apply_layers(&tempdir, &blobs)?;

    let report = lightr_index::snapshot(&tempdir, store, name)?;

    Ok(super::model::ImportReport {
        name: name.to_string(),
        root: report.root,
        layers: layer_count,
        files: report.files,
    })
}
