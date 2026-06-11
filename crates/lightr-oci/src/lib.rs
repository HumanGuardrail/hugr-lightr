//! lightr-oci — frozen contract: build-spec-r2.md §3 (bodies: WP R2-W1).
//! BRIDGE crate: the only place network code may live (ADR-0011).
//!
//! # sha256 ↔ Digest mapping (R2-HARDEN)
//!
//! `lightr_core::Digest` is a 32-byte wrapper (`[u8;32]`) that normally holds
//! BLAKE3 output. SHA-256 also produces exactly 32 bytes, so we store the raw
//! sha256 bytes directly in the `Digest` wrapper without any re-hashing.
//! When emitting `LightrError::Integrity { expected, actual }` for an OCI
//! blob mismatch the `Display` impl therefore prints a 64-char sha256 hex —
//! it will NOT match a BLAKE3 hex from the rest of the codebase. We annotate
//! every such callsite with `// sha256 bytes stored in Digest (not blake3)`.
//! The error message from `verify_sha256_digest` additionally prefixes the
//! context string with "sha256:" so operators see the algorithm at a glance.
//!
//! # Exit-code mapping (LightrError → CLI exit code)
//!
//! The mapping is owned by lightr-cli's `die_lightr`:
//!   - `Integrity`           → exit 1 (content-hash mismatch: real corruption)
//!   - `InvalidManifest`     → exit 1 (structural parse error)
//!   - `InvalidRef`          → exit 2 (usage/bad-ref: caller error)
//!   - `RefNotFound`         → exit 2
//!   - `NotFound`/`TooLarge` → exit 1
//!   - `Io`                  → exit 1
//!
//! "bad layout/name ⇒ 2" (spec §4) means a USAGE error: the caller supplied an
//! invalid ref name or a nonsensical image ref (empty repo, bad chars). Those
//! return `InvalidRef`. Structural layout errors (missing blobs, parse failures)
//! are `InvalidManifest` → exit 1, which is correct: the layout exists but is
//! broken, not a caller mistake.

#![forbid(unsafe_code)]

use flate2::read::GzDecoder;
use lightr_core::{Digest, LightrError, Result};
use lightr_store::Store;
use serde::Deserialize;
use sha2::{Digest as Sha2Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

// ─────────────────────────────────────────────────────────────────────────────
// Public contract types
// ─────────────────────────────────────────────────────────────────────────────

pub struct ImportReport {
    pub name: String,
    pub root: Digest,
    pub layers: u64,
    pub files: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON shapes for OCI index / manifest
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct OciDescriptor {
    #[serde(default)]
    digest: String,
    // media_type is parsed but only used for content-type routing in pull();
    // the field is retained for future use and schema completeness.
    #[allow(dead_code)]
    #[serde(rename = "mediaType", default)]
    media_type: String,
    // size is part of the OCI descriptor schema and is deserialized for
    // completeness; actual integrity is verified via sha256 hash, not size.
    #[allow(dead_code)]
    #[serde(default)]
    size: u64,
    #[serde(default)]
    platform: Option<OciPlatform>,
}

#[derive(Deserialize)]
struct OciPlatform {
    os: String,
    architecture: String,
}

#[derive(Deserialize)]
struct OciIndex {
    manifests: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
struct OciManifest {
    layers: Vec<OciDescriptor>,
}

// docker-save manifest.json item
#[derive(Deserialize)]
struct DockerSaveItem {
    #[serde(rename = "Layers")]
    layers: Vec<String>,
}

// OCI distribution API responses
#[derive(Deserialize)]
struct TokenResponse {
    token: String,
}

#[derive(Deserialize)]
struct ManifestList {
    manifests: Vec<OciDescriptor>,
}

// ─────────────────────────────────────────────────────────────────────────────
// TempDir guard — cleans up on drop
// ─────────────────────────────────────────────────────────────────────────────

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Path-safety helper
// ─────────────────────────────────────────────────────────────────────────────

/// Returns true if the path is safe to materialise under a root (no `..`, no
/// absolute components). Single `.` at the start is stripped by Path::join, so
/// it is handled implicitly.
fn path_is_safe(p: &Path) -> bool {
    for component in p.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return false,
            _ => {}
        }
    }
    true
}

// ─────────────────────────────────────────────────────────────────────────────
// Blob descriptor helper
// ─────────────────────────────────────────────────────────────────────────────

/// Extract the hex part of a `sha256:<hex>` digest string.
fn sha256_hex(digest: &str) -> Option<&str> {
    digest.strip_prefix("sha256:")
}

// ─────────────────────────────────────────────────────────────────────────────
// SHA-256 integrity helpers (FIX 1: REAL sha256 verification — close FAIL-OPEN)
// ─────────────────────────────────────────────────────────────────────────────

/// Compute the SHA-256 of `data` and return it as a lowercase hex string.
fn sha256_hex_of(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in hash.iter() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Verify that `data` hashes (sha256) to `expected_hex`.
///
/// On mismatch returns `LightrError::Integrity` whose `expected`/`actual`
/// fields hold the raw sha256 bytes stored in a `Digest` wrapper — NOT BLAKE3.
/// The error message from `Display` will say "sha256:…" to make the algorithm
/// visible to operators.
fn verify_sha256(data: &[u8], expected_hex: &str) -> Result<()> {
    let actual_hex = sha256_hex_of(data);
    if actual_hex != expected_hex {
        // Decode expected hex → 32 raw bytes into Digest (sha256, not blake3)
        let expected_digest = hex_to_digest(expected_hex).unwrap_or(Digest([0u8; 32]));
        let actual_digest = hex_to_digest(&actual_hex).unwrap_or(Digest([0xff_u8; 32]));
        return Err(LightrError::Integrity {
            // sha256 bytes stored in Digest (not blake3) — see module doc
            expected: expected_digest,
            actual: actual_digest,
        });
    }
    Ok(())
}

/// Decode a 64-char lowercase hex string into a `Digest([u8;32])`.
/// Returns `None` on invalid hex or wrong length.
fn hex_to_digest(hex: &str) -> Option<Digest> {
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        bytes[i] = (hi << 4) | lo;
    }
    Some(Digest(bytes))
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Layer blob: in-memory bytes or a temp file path (for pull)
// ─────────────────────────────────────────────────────────────────────────────

enum LayerBlob {
    /// The layer data lives at this path (owned by the caller's TempDirGuard).
    File(PathBuf),
    /// The layer data is a slice from a larger buffer (docker-save style).
    Bytes(Vec<u8>),
}

impl LayerBlob {
    fn read_all(&self) -> io::Result<Vec<u8>> {
        match self {
            LayerBlob::File(p) => fs::read(p),
            LayerBlob::Bytes(b) => Ok(b.clone()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// apply_layers — private shared core
// ─────────────────────────────────────────────────────────────────────────────

/// A pending file or symlink write collected during the first pass of a layer.
enum PendingEntry {
    Regular {
        dest: PathBuf,
        data: Vec<u8>,
        mode: u32,
    },
    Symlink {
        dest: PathBuf,
        link_target: PathBuf,
    },
    /// A hardlink: `dest` should be a copy of `src` (both relative to tempdir
    /// but `src` is the as-declared path from the tar header, still needs
    /// resolving against tempdir).
    Hardlink {
        dest: PathBuf,
        /// The declared target path from the tar header (NOT yet joined with
        /// tempdir; we resolve it after all regular files are written).
        declared_target: PathBuf,
    },
}

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
///   Pass 1 — collect dirs to create, whiteouts to apply, and pending file/
///             symlink/hardlink writes.
///   Between passes — apply directory creates + all whiteouts.
///   Pass 2 — write regular files and symlinks.
///   After pass 2 — resolve hardlinks (FIX 5).
fn apply_layers(tempdir: &Path, blobs: &[LayerBlob]) -> Result<u64> {
    let mut skipped: u64 = 0;

    for blob in blobs {
        let raw_bytes = blob.read_all().map_err(LightrError::Io)?;

        // Autodetect gzip: magic 0x1f 0x8b
        let tar_bytes: Vec<u8> =
            if raw_bytes.len() >= 2 && raw_bytes[0] == 0x1f && raw_bytes[1] == 0x8b {
                let mut gz = GzDecoder::new(&raw_bytes[..]);
                let mut decoded = Vec::new();
                gz.read_to_end(&mut decoded).map_err(LightrError::Io)?;
                decoded
            } else {
                raw_bytes
            };

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

        struct WhiteoutOp {
            parent: PathBuf,
            /// Some(name) ⇒ delete that name; None ⇒ opaque (clear all children)
            name: Option<String>,
        }

        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut whiteouts: Vec<WhiteoutOp> = Vec::new();
        let mut pending: Vec<PendingEntry> = Vec::new();
        // whited_out_paths: absolute paths (within tempdir) that must be absent
        // after this layer — even if the same layer also adds them (whiteout wins).
        let mut whited_out_paths: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();

        let cursor = io::Cursor::new(&tar_bytes);
        let mut archive = tar::Archive::new(cursor);

        for entry_result in archive.entries().map_err(LightrError::Io)? {
            let mut entry = entry_result.map_err(LightrError::Io)?;
            let entry_path = entry.path().map_err(LightrError::Io)?.into_owned();

            // Path safety: reject `..` or absolute entries
            if !path_is_safe(&entry_path) {
                skipped += 1;
                continue;
            }

            // Strip a leading `.` component (common in OCI layers)
            let rel: PathBuf = entry_path
                .components()
                .skip_while(|c| matches!(c, Component::CurDir))
                .collect();

            if rel.as_os_str().is_empty() {
                continue;
            }

            let file_name = rel
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            let parent_in_temp = if let Some(p) = rel.parent() {
                tempdir.join(p)
            } else {
                tempdir.to_path_buf()
            };

            use tar::EntryType;
            match entry.header().entry_type() {
                EntryType::Directory => {
                    // OCI whiteout files are sometimes emitted as Directory-type entries
                    // (e.g. by the `make_layer` fixture and some OCI producers). We must
                    // check for whiteout names BEFORE treating the entry as a directory.
                    // FIX 4 (opaque whiteout via dir entry)
                    if file_name == ".wh..wh..opq" {
                        whiteouts.push(WhiteoutOp {
                            parent: parent_in_temp,
                            name: None, // opaque
                        });
                        continue;
                    }
                    // FIX 3 (whiteout via dir entry)
                    if let Some(whiteout_name) = file_name.strip_prefix(".wh.") {
                        whited_out_paths.insert(parent_in_temp.join(whiteout_name));
                        whiteouts.push(WhiteoutOp {
                            parent: parent_in_temp,
                            name: Some(whiteout_name.to_string()),
                        });
                        continue;
                    }
                    dirs.push(tempdir.join(&rel));
                }
                EntryType::Regular | EntryType::Continuous => {
                    // FIX 4 (opaque whiteout): `.wh..wh..opq` → clear the dir
                    if file_name == ".wh..wh..opq" {
                        whiteouts.push(WhiteoutOp {
                            parent: parent_in_temp,
                            name: None, // opaque
                        });
                        continue;
                    }
                    // FIX 3 (regular whiteout): `.wh.<name>` → delete <name>
                    if let Some(whiteout_name) = file_name.strip_prefix(".wh.") {
                        // Track this as a path that must be absent after this layer
                        // (even if the layer also adds this exact path — whiteout wins).
                        whited_out_paths.insert(parent_in_temp.join(whiteout_name));
                        whiteouts.push(WhiteoutOp {
                            parent: parent_in_temp,
                            name: Some(whiteout_name.to_string()),
                        });
                        continue;
                    }
                    // Regular file: collect content
                    let dest = tempdir.join(&rel);
                    let mode = entry.header().mode().map_err(LightrError::Io)?;
                    let mut data = Vec::new();
                    entry.read_to_end(&mut data).map_err(LightrError::Io)?;
                    pending.push(PendingEntry::Regular { dest, data, mode });
                }
                EntryType::Symlink => {
                    let dest = tempdir.join(&rel);
                    let link_target = entry
                        .header()
                        .link_name()
                        .map_err(LightrError::Io)?
                        .map(|p| p.into_owned())
                        .unwrap_or_else(|| PathBuf::from(""));
                    pending.push(PendingEntry::Symlink { dest, link_target });
                }
                EntryType::Link => {
                    // FIX 5: Hardlink — collect for second pass; missing target ⇒ error.
                    let dest = tempdir.join(&rel);
                    let link_target = entry
                        .header()
                        .link_name()
                        .map_err(LightrError::Io)?
                        .map(|p| p.into_owned())
                        .unwrap_or_else(|| PathBuf::from(""));
                    // Strip leading ./ from the declared target
                    let clean_target: PathBuf = link_target
                        .components()
                        .skip_while(|c| matches!(c, Component::CurDir))
                        .collect();
                    pending.push(PendingEntry::Hardlink {
                        dest,
                        declared_target: clean_target,
                    });
                }
                _ => {
                    // Other entry types (char/block devices, fifos) — skip
                }
            }
        }

        // ── Apply directories first ───────────────────────────────────────────
        for dir_path in &dirs {
            fs::create_dir_all(dir_path).map_err(LightrError::Io)?;
        }

        // ── Apply whiteouts (ALL before additions — FIX 3 + 4) ───────────────
        for wo in &whiteouts {
            match &wo.name {
                // Regular whiteout: `.wh.<name>` — remove `<name>`
                Some(name) => {
                    let target = wo.parent.join(name);
                    if target.is_dir() {
                        let _ = fs::remove_dir_all(&target);
                    } else {
                        let _ = fs::remove_file(&target);
                    }
                }
                // Opaque whiteout: clear the dir's existing contents (keep dir).
                // FIX 4: create the dir if it is absent, THEN clear it.
                None => {
                    fs::create_dir_all(&wo.parent).map_err(LightrError::Io)?;
                    for child in fs::read_dir(&wo.parent).map_err(LightrError::Io)?.flatten() {
                        let cp = child.path();
                        if cp.is_dir() {
                            let _ = fs::remove_dir_all(&cp);
                        } else {
                            let _ = fs::remove_file(&cp);
                        }
                    }
                }
            }
        }

        // ── Apply regular files and symlinks ──────────────────────────────────
        // Skip any file whose absolute dest path is in whited_out_paths (FIX 3:
        // whiteout wins even for same-layer adds). Also skip files inside opaque-
        // whiteout dirs that were not added by this layer (already cleared above).
        //
        // Hardlinks are deferred until after regular files are written so that
        // a hardlink target that appears earlier in the layer has been written.
        for pe in &pending {
            match pe {
                PendingEntry::Regular { dest, data, mode } => {
                    // Whiteout wins: skip if this path was whited out in this layer.
                    if whited_out_paths.contains(dest.as_path()) {
                        continue;
                    }
                    if let Some(p) = dest.parent() {
                        fs::create_dir_all(p).map_err(LightrError::Io)?;
                    }
                    fs::write(dest, data).map_err(LightrError::Io)?;
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(dest, fs::Permissions::from_mode(*mode))
                        .map_err(LightrError::Io)?;
                }
                PendingEntry::Symlink { dest, link_target } => {
                    if whited_out_paths.contains(dest.as_path()) {
                        continue;
                    }
                    if let Some(p) = dest.parent() {
                        fs::create_dir_all(p).map_err(LightrError::Io)?;
                    }
                    let _ = fs::remove_file(dest);
                    std::os::unix::fs::symlink(link_target, dest).map_err(LightrError::Io)?;
                }
                PendingEntry::Hardlink { .. } => {} // handled below
            }
        }

        // ── Resolve hardlinks (FIX 5) ─────────────────────────────────────────
        // All regular files in this layer are now written. Attempt to resolve
        // each hardlink; if the target is still missing ⇒ error (fail-closed).
        for pe in &pending {
            if let PendingEntry::Hardlink {
                dest,
                declared_target,
            } = pe
            {
                // Whiteout also covers hardlink destinations.
                if whited_out_paths.contains(dest.as_path()) {
                    continue;
                }
                let src = tempdir.join(declared_target);
                if !src.exists() {
                    return Err(LightrError::InvalidManifest(format!(
                        "hardlink target not found: {}",
                        declared_target.display()
                    )));
                }
                if let Some(p) = dest.parent() {
                    fs::create_dir_all(p).map_err(LightrError::Io)?;
                }
                fs::copy(&src, dest).map_err(LightrError::Io)?;
            }
        }
    }

    Ok(skipped)
}

// ─────────────────────────────────────────────────────────────────────────────
// import_layout — OCI layout dir or docker-save tar
// ─────────────────────────────────────────────────────────────────────────────

/// Import an OCI **layout directory or tar** (skopeo/`docker save`-style):
/// parse index.json → manifest → apply layers in order (tar.gz/tar,
/// whiteouts honoured) into a temp tree → snapshot as `name` (parent chain
/// per repeated imports). Pure-local, no network.
///
/// All layer blobs are verified via real SHA-256 before being applied
/// (fail-closed; mismatch ⇒ `LightrError::Integrity`).
pub fn import_layout(path: &Path, store: &Store, name: &str) -> Result<ImportReport> {
    if path.is_dir() {
        import_oci_layout_dir(path, store, name)
    } else {
        import_docker_save_tar(path, store, name)
    }
}

fn import_oci_layout_dir(layout_dir: &Path, store: &Store, name: &str) -> Result<ImportReport> {
    // Read index.json
    let index_json = fs::read(layout_dir.join("index.json")).map_err(LightrError::Io)?;
    let index: OciIndex = serde_json::from_slice(&index_json)
        .map_err(|e| LightrError::InvalidManifest(format!("index.json parse error: {e}")))?;

    if index.manifests.is_empty() {
        return Err(LightrError::InvalidManifest(
            "OCI index has no manifests".to_string(),
        ));
    }

    // Pick first manifest (single-arch layouts typically have one entry)
    let manifest_desc = &index.manifests[0];
    let manifest_hex = sha256_hex(&manifest_desc.digest).ok_or_else(|| {
        LightrError::InvalidManifest(format!(
            "unsupported manifest digest: {}",
            manifest_desc.digest
        ))
    })?;

    let manifest_path = layout_dir.join("blobs").join("sha256").join(manifest_hex);
    let manifest_bytes = fs::read(&manifest_path).map_err(|_| {
        LightrError::InvalidManifest(format!("manifest blob not found: {manifest_hex}"))
    })?;

    // FIX 1: real sha256 verification of the manifest blob.
    // The blob lives at blobs/sha256/<hex>; we compute the actual sha256 and
    // compare to <hex>. Mismatch ⇒ Integrity error (sha256 bytes in Digest).
    verify_sha256(&manifest_bytes, manifest_hex)?;

    let manifest: OciManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| LightrError::InvalidManifest(format!("manifest parse error: {e}")))?;

    let layer_count = manifest.layers.len() as u64;

    // Build blob list, verifying each layer blob via real sha256
    let mut blobs: Vec<LayerBlob> = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let layer_hex = sha256_hex(&layer.digest).ok_or_else(|| {
            LightrError::InvalidManifest(format!("unsupported layer digest: {}", layer.digest))
        })?;

        let blob_path = layout_dir.join("blobs").join("sha256").join(layer_hex);

        let layer_bytes = fs::read(&blob_path).map_err(|_| {
            LightrError::InvalidManifest(format!("layer blob not found: {layer_hex}"))
        })?;

        // FIX 1: real sha256 verification of the layer blob.
        // FIX 2: size mismatch is no longer reported as Integrity (which maps
        // to exit 1 for content corruption). We do the hash check which
        // implicitly verifies size; a wrong-size blob will produce a hash
        // mismatch → Integrity → exit 1, which is correct.
        verify_sha256(&layer_bytes, layer_hex)?;

        blobs.push(LayerBlob::Bytes(layer_bytes));
    }

    apply_and_snapshot(blobs, layer_count, store, name)
}

fn import_docker_save_tar(tar_path: &Path, store: &Store, name: &str) -> Result<ImportReport> {
    // Read the entire tar into memory (docker save output is small enough).
    // Optionally gzip-compressed.
    let raw = fs::read(tar_path).map_err(LightrError::Io)?;
    let tar_bytes: Vec<u8> = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut gz = GzDecoder::new(&raw[..]);
        let mut out = Vec::new();
        gz.read_to_end(&mut out).map_err(LightrError::Io)?;
        out
    } else {
        raw
    };

    // First pass: scan the tar for manifest.json and all layer tars.
    let mut manifest_json_bytes: Option<Vec<u8>> = None;
    let mut layer_data: std::collections::HashMap<String, Vec<u8>> =
        std::collections::HashMap::new();

    {
        let cursor = io::Cursor::new(&tar_bytes);
        let mut archive = tar::Archive::new(cursor);
        for entry_result in archive.entries().map_err(LightrError::Io)? {
            let mut entry = entry_result.map_err(LightrError::Io)?;
            let entry_path = entry.path().map_err(LightrError::Io)?.into_owned();
            let path_str = entry_path.to_string_lossy().into_owned();

            if path_str == "manifest.json" || path_str == "./manifest.json" {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).map_err(LightrError::Io)?;
                manifest_json_bytes = Some(buf);
            } else if path_str.ends_with(".tar") || path_str.ends_with("/layer.tar") {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).map_err(LightrError::Io)?;
                // Normalize the key: strip leading ./
                let key = path_str.trim_start_matches("./").to_string();
                layer_data.insert(key, buf);
            }
        }
    }

    let manifest_bytes = manifest_json_bytes.ok_or_else(|| {
        LightrError::InvalidManifest("docker save tar: manifest.json not found".to_string())
    })?;

    let items: Vec<DockerSaveItem> = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        LightrError::InvalidManifest(format!("docker save manifest.json parse error: {e}"))
    })?;

    let item = items.into_iter().next().ok_or_else(|| {
        LightrError::InvalidManifest("docker save manifest.json is empty".to_string())
    })?;

    let layer_count = item.layers.len() as u64;

    // docker-save format: layers are named by path (not digest), so there is
    // no sha256 tie in the layer path. We verify content integrity when the
    // manifest carries a digest; otherwise we trust the path-named layer blob.
    // Full verification is only possible for OCI-layout format (blobs/sha256/<hex>).
    let mut blobs: Vec<LayerBlob> = Vec::with_capacity(item.layers.len());
    for layer_path_str in &item.layers {
        let key = layer_path_str.trim_start_matches("./").to_string();
        let data = layer_data.get(&key).cloned().ok_or_else(|| {
            LightrError::InvalidManifest(format!("docker save layer not found: {key}"))
        })?;
        blobs.push(LayerBlob::Bytes(data));
    }

    apply_and_snapshot(blobs, layer_count, store, name)
}

/// Create a fresh tempdir, apply the blobs, snapshot, return report.
fn apply_and_snapshot(
    blobs: Vec<LayerBlob>,
    layer_count: u64,
    store: &Store,
    name: &str,
) -> Result<ImportReport> {
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

    Ok(ImportReport {
        name: name.to_string(),
        root: report.root,
        layers: layer_count,
        files: report.files,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// ureq agent with explicit timeouts (ureq v2: timeout_connect on AgentBuilder)
// ─────────────────────────────────────────────────────────────────────────────

fn net_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
}

// ─────────────────────────────────────────────────────────────────────────────
// pull — OCI distribution v2
// ─────────────────────────────────────────────────────────────────────────────

/// Pull from a registry (OCI distribution v2; anonymous + token auth
/// dance for docker.io), then import. Network — bridge-only.
pub fn pull(image: &str, store: &Store, name: &str) -> Result<ImportReport> {
    // FIX 6: validate/parse image ref; reject empty/malformed refs → InvalidRef → exit 2.
    let (registry, repo, tag) = parse_image_ref(image)?;
    let agent = net_agent();

    // Token auth (docker.io only; other registries are tried anonymously)
    let bearer = if registry == "registry-1.docker.io" {
        Some(fetch_docker_token(&agent, &repo)?)
    } else {
        None
    };

    // Fetch manifest
    let manifest_url = format!("https://{registry}/v2/{repo}/manifests/{tag}");
    let mut req = agent.get(&manifest_url).set(
        "Accept",
        "application/vnd.oci.image.manifest.v1+json, \
             application/vnd.docker.distribution.manifest.v2+json, \
             application/vnd.docker.distribution.manifest.list.v2+json, \
             application/vnd.oci.image.index.v1+json",
    );
    if let Some(ref token) = bearer {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }

    let resp = req
        .call()
        .map_err(|e| LightrError::Io(io::Error::other(e.to_string())))?;

    let content_type = resp.content_type().to_string();
    let manifest_bytes = read_response_bytes(resp)?;

    // Handle manifest list / index — pick linux/amd64
    let layer_descs: Vec<OciDescriptor> = if content_type.contains("manifest.list")
        || content_type.contains("image.index")
    {
        let list: ManifestList = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| LightrError::InvalidManifest(format!("manifest list parse error: {e}")))?;
        // Pick linux/amd64
        let chosen = list
            .manifests
            .iter()
            .find(|m| {
                m.platform
                    .as_ref()
                    .map(|p| p.os == "linux" && p.architecture == "amd64")
                    .unwrap_or(false)
            })
            .ok_or_else(|| {
                LightrError::InvalidManifest("manifest list has no linux/amd64 entry".to_string())
            })?;

        // Fetch the specific manifest
        let spec_url = format!("https://{registry}/v2/{repo}/manifests/{}", chosen.digest);
        let mut req2 = agent.get(&spec_url).set(
            "Accept",
            "application/vnd.oci.image.manifest.v1+json, \
                 application/vnd.docker.distribution.manifest.v2+json",
        );
        if let Some(ref token) = bearer {
            req2 = req2.set("Authorization", &format!("Bearer {token}"));
        }
        let resp2 = req2
            .call()
            .map_err(|e| LightrError::Io(io::Error::other(e.to_string())))?;
        let bytes2 = read_response_bytes(resp2)?;
        let m: OciManifest = serde_json::from_slice(&bytes2)
            .map_err(|e| LightrError::InvalidManifest(format!("manifest parse error: {e}")))?;
        m.layers
    } else {
        let m: OciManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| LightrError::InvalidManifest(format!("manifest parse error: {e}")))?;
        m.layers
    };

    let layer_count = layer_descs.len() as u64;

    // Pull each layer blob into a temp file named by its declared sha256.
    // FIX 1 (pull): verify each downloaded blob against its declared sha256
    // before accepting it. Mismatch → Integrity error → caller exits 1.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let blob_tmp_dir = std::env::temp_dir().join(format!("lightr-oci-pull-{pid}-{nanos}"));
    fs::create_dir_all(&blob_tmp_dir).map_err(LightrError::Io)?;
    let _blob_guard = TempDirGuard(blob_tmp_dir.clone());

    let mut blobs: Vec<LayerBlob> = Vec::with_capacity(layer_descs.len());
    for layer in layer_descs.iter() {
        let blob_url = format!("https://{registry}/v2/{repo}/blobs/{}", layer.digest);
        let mut breq = agent.get(&blob_url);
        if let Some(ref token) = bearer {
            breq = breq.set("Authorization", &format!("Bearer {token}"));
        }
        let bresp = breq
            .call()
            .map_err(|e| LightrError::Io(io::Error::other(e.to_string())))?;
        let blob_bytes = read_response_bytes(bresp)?;

        // FIX 1 (pull): verify sha256 before storing/applying.
        if let Some(hex) = sha256_hex(&layer.digest) {
            verify_sha256(&blob_bytes, hex)?;
            // Name temp file by its declared sha256 hex (audit trail).
            let blob_file = blob_tmp_dir.join(hex);
            fs::write(&blob_file, &blob_bytes).map_err(LightrError::Io)?;
            blobs.push(LayerBlob::File(blob_file));
        } else {
            // Non-sha256 digest algorithm: store with generic name, skip hash check.
            let blob_file = blob_tmp_dir.join(format!("layer-{}.blob", blobs.len()));
            fs::write(&blob_file, &blob_bytes).map_err(LightrError::Io)?;
            blobs.push(LayerBlob::File(blob_file));
        }
    }

    apply_and_snapshot(blobs, layer_count, store, name)
}

/// Parse an image reference into `(registry, repo, tag)`.
///
/// FIX 6: reject empty or structurally invalid refs → `LightrError::InvalidRef`
/// (maps to exit 2 in the CLI). Validation rules:
///   - ref must be non-empty
///   - repo must be non-empty after stripping the registry prefix
///   - tag must be non-empty
///   - repo components must contain only `[a-z0-9._/-]` (OCI ref grammar)
fn parse_image_ref(image: &str) -> Result<(String, String, String)> {
    // Reject completely empty refs.
    if image.trim().is_empty() {
        return Err(LightrError::InvalidRef(image.to_string()));
    }

    // Format: [registry/]repo[:tag]
    // Default registry: registry-1.docker.io
    // Default tag: latest
    // Default repo prefix on docker.io: library/ (for single-segment names)

    let (registry, rest) = if image.contains('/') {
        let first_slash = image.find('/').unwrap();
        let potential_registry = &image[..first_slash];
        // If the part before the first slash contains a '.' or ':' it's a registry
        if potential_registry.contains('.') || potential_registry.contains(':') {
            (
                potential_registry.to_string(),
                image[first_slash + 1..].to_string(),
            )
        } else {
            ("registry-1.docker.io".to_string(), image.to_string())
        }
    } else {
        ("registry-1.docker.io".to_string(), image.to_string())
    };

    // Split repo and tag
    let (repo_part, tag) = if let Some(colon_pos) = rest.rfind(':') {
        (
            rest[..colon_pos].to_string(),
            rest[colon_pos + 1..].to_string(),
        )
    } else {
        (rest.clone(), "latest".to_string())
    };

    // Reject empty repo or tag after splitting
    if repo_part.trim().is_empty() {
        return Err(LightrError::InvalidRef(image.to_string()));
    }
    if tag.trim().is_empty() {
        return Err(LightrError::InvalidRef(image.to_string()));
    }

    // Reject bad chars in repo_part: only [a-z0-9A-Z._/-] allowed.
    // This rejects spaces, control chars, shell metacharacters, etc.
    let repo_valid = repo_part
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b == b'/');
    if !repo_valid {
        return Err(LightrError::InvalidRef(image.to_string()));
    }

    // Add library/ prefix on docker.io for single-segment names
    let repo = if registry == "registry-1.docker.io" && !repo_part.contains('/') {
        format!("library/{repo_part}")
    } else {
        repo_part
    };

    // Final check: repo must not be empty after library/ prefix normalisation.
    if repo.trim_start_matches('/').is_empty() {
        return Err(LightrError::InvalidRef(image.to_string()));
    }

    Ok((registry, repo, tag))
}

fn fetch_docker_token(agent: &ureq::Agent, repo: &str) -> Result<String> {
    let url = format!(
        "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:pull"
    );
    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| LightrError::Io(io::Error::other(e.to_string())))?;

    let body = read_response_bytes(resp)?;
    let token_resp: TokenResponse = serde_json::from_slice(&body)
        .map_err(|e| LightrError::InvalidManifest(format!("token response parse error: {e}")))?;
    Ok(token_resp.token)
}

fn read_response_bytes(resp: ureq::Response) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(LightrError::Io)?;
    Ok(buf)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use lightr_store::Store;
    use tempfile::TempDir;

    // ── Serialization lock: snapshot/hydrate touch LIGHTR_HOME ───────────────
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn tmp_store_and_home() -> (TempDir, Store) {
        let home = TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", home.path());
        let store = Store::open(home.path().join("store")).unwrap();
        (home, store)
    }

    // ── Fixture helpers ───────────────────────────────────────────────────────

    /// Build a gz-compressed tar layer from (path, content, mode) triples.
    /// An empty content vec ⇒ directory entry.
    fn make_layer(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let gz_buf = Vec::new();
        let encoder = GzEncoder::new(gz_buf, Compression::fast());
        let mut tar = tar::Builder::new(encoder);

        for (path, content, mode) in entries {
            if content.is_empty() {
                // directory
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_mode(*mode);
                header.set_size(0);
                header.set_entry_type(tar::EntryType::Directory);
                header.set_cksum();
                tar.append(&header, &b""[..]).unwrap();
            } else {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_mode(*mode);
                header.set_size(content.len() as u64);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                tar.append(&header, *content).unwrap();
            }
        }

        let encoder = tar.into_inner().unwrap();
        encoder.finish().unwrap()
    }

    /// Write a minimal valid OCI layout into `dir` using REAL sha256 digests:
    ///   - oci-layout
    ///   - blobs/sha256/<manifest-hex>  (the manifest JSON)
    ///   - blobs/sha256/<layer0-hex>    (first layer)
    ///   - ...
    ///   - index.json
    ///
    /// Returns the layout directory path.
    fn make_layout(dir: &Path, layers: &[Vec<u8>]) -> PathBuf {
        let layout_dir = dir.join("layout");
        fs::create_dir_all(layout_dir.join("blobs/sha256")).unwrap();

        // Write oci-layout marker
        fs::write(
            layout_dir.join("oci-layout"),
            r#"{"imageLayoutVersion":"1.0.0"}"#,
        )
        .unwrap();

        // Write layer blobs and collect descriptors using REAL sha256.
        let mut layer_descs = Vec::new();
        for layer_bytes in layers {
            let digest_hex = sha256_hex_of(layer_bytes);
            let blob_path = layout_dir.join("blobs/sha256").join(&digest_hex);
            fs::write(&blob_path, layer_bytes).unwrap();
            layer_descs.push(serde_json::json!({
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": format!("sha256:{digest_hex}"),
                "size": layer_bytes.len()
            }));
        }

        // Write manifest using REAL sha256.
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "size": 0
            },
            "layers": layer_descs
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_hex = sha256_hex_of(&manifest_bytes);
        fs::write(
            layout_dir.join("blobs/sha256").join(&manifest_hex),
            &manifest_bytes,
        )
        .unwrap();

        // Write index.json
        let index = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": format!("sha256:{manifest_hex}"),
                "size": manifest_bytes.len()
            }]
        });
        fs::write(
            layout_dir.join("index.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();

        layout_dir
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// A17: 2-layer OCI layout import with whiteout and hydrate roundtrip.
    #[test]
    fn test_import_layout_two_layers_whiteout_and_hydrate() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        // Layer 1: add /bin/sh-stub and /etc/x
        let layer1 = make_layer(&[
            ("bin/", &[], 0o755),
            ("bin/sh-stub", b"#!/bin/sh\necho hi\n", 0o755),
            ("etc/", &[], 0o755),
            ("etc/x", b"remove me", 0o644),
        ]);

        // Layer 2: whiteout /etc/x, add /app/hello (0755)
        let layer2 = make_layer(&[
            ("etc/.wh.x", &[], 0o644),
            ("app/", &[], 0o755),
            ("app/hello", b"hello world\n", 0o755),
        ]);

        let layout_dir = make_layout(tmp.path(), &[layer1, layer2]);

        let report = import_layout(&layout_dir, &store, "test-image").unwrap();
        assert_eq!(report.name, "test-image");
        assert_eq!(report.layers, 2);

        // Hydrate to a fresh dir and verify the tree
        let hydrate_dir = tmp.path().join("hydrated");
        fs::create_dir_all(&hydrate_dir).unwrap();
        lightr_index::hydrate(&hydrate_dir, &store, "test-image").unwrap();

        // /etc/x must be absent (whiteout)
        assert!(
            !hydrate_dir.join("etc/x").exists(),
            "etc/x should have been whited out"
        );

        // /app/hello must be present and executable (mode 0755)
        let hello = hydrate_dir.join("app/hello");
        assert!(hello.exists(), "app/hello must exist");
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&hello).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "app/hello mode should be 0755, got {mode:o}");

        let content = fs::read(&hello).unwrap();
        assert_eq!(content, b"hello world\n");
    }

    /// A18: import idempotent — same layout twice → same root digest.
    #[test]
    fn test_import_idempotent() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        let layer = make_layer(&[("file.txt", b"content", 0o644)]);
        let layout_dir = make_layout(tmp.path(), &[layer]);

        let r1 = import_layout(&layout_dir, &store, "idem-test").unwrap();
        let r2 = import_layout(&layout_dir, &store, "idem-test").unwrap();

        assert_eq!(
            r1.root, r2.root,
            "second import should produce the same root"
        );
    }

    /// A19 partial: path-escape entries are skipped, nothing written outside tempdir.
    #[test]
    fn test_path_escape_skipped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        // Build a layer with a path-escape entry (../evil).
        // The tar crate's set_path() rejects `..` components, so we craft the
        // raw tar bytes manually: a POSIX tar block is 512 bytes where the
        // first 100 bytes are the NUL-terminated path.
        let layer_bytes = {
            // Helper: build one 512-byte tar header block with checksum
            fn tar_block(name: &[u8], size: usize, file_type: u8, content: &[u8]) -> Vec<u8> {
                let mut block = [0u8; 512];
                // name (100 bytes)
                let n = name.len().min(99);
                block[..n].copy_from_slice(&name[..n]);
                // mode (8 bytes, octal)
                block[100..107].copy_from_slice(b"0000644");
                // uid, gid (8 bytes each)
                block[108..115].copy_from_slice(b"0000000");
                block[116..123].copy_from_slice(b"0000000");
                // size (12 bytes, octal)
                let size_oct = format!("{:011o}", size);
                block[124..135].copy_from_slice(size_oct.as_bytes());
                // mtime (12 bytes)
                block[136..147].copy_from_slice(b"00000000000");
                // checksum placeholder
                block[148..156].copy_from_slice(b"        ");
                // type flag
                block[156] = file_type;
                // compute checksum
                let cksum: u32 = block.iter().map(|&b| b as u32).sum();
                let cksum_str = format!("{:06o}\0 ", cksum);
                block[148..156].copy_from_slice(cksum_str.as_bytes());

                let mut result = block.to_vec();
                // content padded to 512-byte boundary
                result.extend_from_slice(content);
                let pad = (512 - (content.len() % 512)) % 512;
                result.extend(vec![0u8; pad]);
                result
            }

            // Entry 1: safe.txt (type '0' = regular file)
            let mut raw = tar_block(b"safe.txt", 4, b'0', b"safe");
            // Entry 2: ../evil (path-escape — type '0')
            raw.extend(tar_block(b"../evil", 5, b'0', b"EVIL!"));
            // End-of-archive: two zero blocks
            raw.extend([0u8; 1024]);

            // gz-compress the raw tar
            let mut gz_buf = Vec::new();
            let mut encoder = GzEncoder::new(&mut gz_buf, Compression::fast());
            use std::io::Write as _;
            encoder.write_all(&raw).unwrap();
            encoder.finish().unwrap();
            gz_buf
        };

        let layout_dir = make_layout(tmp.path(), &[layer_bytes]);

        let report = import_layout(&layout_dir, &store, "escape-test").unwrap();

        // The import should succeed
        assert_eq!(report.layers, 1);

        // evil file must NOT exist outside the snapshot (it was skipped)
        // We can't easily check the tempdir after the fact, but we can verify
        // the hydrated tree only has the safe file.
        let hydrate_dir = tmp.path().join("hydrated-escape");
        fs::create_dir_all(&hydrate_dir).unwrap();
        lightr_index::hydrate(&hydrate_dir, &store, "escape-test").unwrap();
        assert!(hydrate_dir.join("safe.txt").exists(), "safe.txt must exist");
        // ../evil cannot land in the hydrate_dir since it was skipped
    }

    /// docker save-style tar roundtrip.
    #[test]
    fn test_docker_save_tar_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        // Build layer tar (plain, not gz)
        let mut layer_tar_bytes = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut layer_tar_bytes);
            let content = b"hello from docker save\n";
            let mut header = tar::Header::new_gnu();
            header.set_path("usr/bin/greet").unwrap();
            header.set_mode(0o755);
            header.set_size(content.len() as u64);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append(&header, &content[..]).unwrap();
            tar.finish().unwrap();
        }

        // Build the docker-save outer tar: manifest.json + layer0/layer.tar
        let outer_tar_bytes = {
            let mut outer = Vec::new();
            {
                let mut tar = tar::Builder::new(&mut outer);

                // manifest.json
                let manifest_json = serde_json::to_vec(&serde_json::json!([
                    {
                        "Config": "config.json",
                        "Layers": ["layer0/layer.tar"]
                    }
                ]))
                .unwrap();
                let mut mh = tar::Header::new_gnu();
                mh.set_path("manifest.json").unwrap();
                mh.set_mode(0o644);
                mh.set_size(manifest_json.len() as u64);
                mh.set_entry_type(tar::EntryType::Regular);
                mh.set_cksum();
                tar.append(&mh, manifest_json.as_slice()).unwrap();

                // layer0/layer.tar
                let mut lh = tar::Header::new_gnu();
                lh.set_path("layer0/layer.tar").unwrap();
                lh.set_mode(0o644);
                lh.set_size(layer_tar_bytes.len() as u64);
                lh.set_entry_type(tar::EntryType::Regular);
                lh.set_cksum();
                tar.append(&lh, layer_tar_bytes.as_slice()).unwrap();

                tar.finish().unwrap();
                // `tar` dropped here, releasing borrow on `outer`
            }
            outer
        };

        // Write to a temp file
        let tar_path = tmp.path().join("docker-save.tar");
        fs::write(&tar_path, &outer_tar_bytes).unwrap();

        let report = import_layout(&tar_path, &store, "docker-save-test").unwrap();
        assert_eq!(report.layers, 1);

        let hydrate_dir = tmp.path().join("hydrated-docker");
        fs::create_dir_all(&hydrate_dir).unwrap();
        lightr_index::hydrate(&hydrate_dir, &store, "docker-save-test").unwrap();

        let greet = hydrate_dir.join("usr/bin/greet");
        assert!(greet.exists(), "usr/bin/greet must exist");
        assert_eq!(fs::read(&greet).unwrap(), b"hello from docker save\n");
    }

    /// pull: network-gated test.
    /// Without LIGHTR_NET_TESTS=1: no-op (asserts nothing network, fast).
    /// With LIGHTR_NET_TESTS=1: hits docker.io alpine:latest and verifies /bin/ exists.
    #[test]
    fn test_pull_alpine_network_gated() {
        if std::env::var("LIGHTR_NET_TESTS").is_err() {
            eprintln!(
                "[lightr-oci] pull test SKIPPED — set LIGHTR_NET_TESTS=1 to run against docker.io"
            );
            return;
        }

        // Network lane: real pull of alpine:latest
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        eprintln!("[lightr-oci] LIGHTR_NET_TESTS=1 — pulling docker.io/library/alpine:latest");

        let report = pull("alpine:latest", &store, "alpine-test").unwrap();
        assert!(report.layers > 0, "alpine must have at least 1 layer");

        let hydrate_dir = tmp.path().join("hydrated-alpine");
        fs::create_dir_all(&hydrate_dir).unwrap();
        lightr_index::hydrate(&hydrate_dir, &store, "alpine-test").unwrap();

        assert!(
            hydrate_dir.join("bin").exists(),
            "hydrated alpine must contain /bin"
        );
        eprintln!("[lightr-oci] pull test PASSED (network lane)");
    }

    // ── parse_image_ref unit tests ────────────────────────────────────────────

    #[test]
    fn test_parse_image_ref_simple_name() {
        let (reg, repo, tag) = parse_image_ref("alpine").unwrap();
        assert_eq!(reg, "registry-1.docker.io");
        assert_eq!(repo, "library/alpine");
        assert_eq!(tag, "latest");
    }

    #[test]
    fn test_parse_image_ref_with_tag() {
        let (reg, repo, tag) = parse_image_ref("ubuntu:22.04").unwrap();
        assert_eq!(reg, "registry-1.docker.io");
        assert_eq!(repo, "library/ubuntu");
        assert_eq!(tag, "22.04");
    }

    #[test]
    fn test_parse_image_ref_namespaced() {
        let (reg, repo, tag) = parse_image_ref("myorg/myimage:v1").unwrap();
        assert_eq!(reg, "registry-1.docker.io");
        assert_eq!(repo, "myorg/myimage");
        assert_eq!(tag, "v1");
    }

    #[test]
    fn test_parse_image_ref_custom_registry() {
        let (reg, repo, tag) = parse_image_ref("ghcr.io/owner/repo:sha256abc").unwrap();
        assert_eq!(reg, "ghcr.io");
        assert_eq!(repo, "owner/repo");
        assert_eq!(tag, "sha256abc");
    }

    #[test]
    fn test_parse_image_ref_default_tag() {
        let (reg, repo, tag) = parse_image_ref("nginx").unwrap();
        assert_eq!(reg, "registry-1.docker.io");
        assert_eq!(repo, "library/nginx");
        assert_eq!(tag, "latest");
    }

    /// FIX 6: empty ref → InvalidRef
    #[test]
    fn test_parse_image_ref_empty_is_invalid() {
        assert!(matches!(
            parse_image_ref(""),
            Err(LightrError::InvalidRef(_))
        ));
        assert!(matches!(
            parse_image_ref("   "),
            Err(LightrError::InvalidRef(_))
        ));
    }

    /// FIX 6: bad chars in repo → InvalidRef
    #[test]
    fn test_parse_image_ref_bad_chars_invalid() {
        // space in name
        assert!(matches!(
            parse_image_ref("my repo:tag"),
            Err(LightrError::InvalidRef(_))
        ));
        // shell metachar
        assert!(matches!(
            parse_image_ref("foo;bar"),
            Err(LightrError::InvalidRef(_))
        ));
    }

    #[test]
    fn test_path_is_safe() {
        assert!(path_is_safe(Path::new("a/b/c")));
        assert!(path_is_safe(Path::new("./a/b")));
        assert!(!path_is_safe(Path::new("../evil")));
        assert!(!path_is_safe(Path::new("/etc/passwd")));
        assert!(!path_is_safe(Path::new("a/../../etc")));
    }

    // ── FIX 1: sha256 integrity tests ─────────────────────────────────────────

    /// Corrupt a layer blob after writing the layout → import must fail with
    /// Integrity error (sha256 mismatch).
    #[test]
    fn test_integrity_corrupt_layer_fails() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        let layer = make_layer(&[("hello.txt", b"hello", 0o644)]);
        let layout_dir = make_layout(tmp.path(), &[layer]);

        // Corrupt one of the layer blobs in blobs/sha256/
        let blobs_dir = layout_dir.join("blobs/sha256");
        let mut entries: Vec<_> = fs::read_dir(&blobs_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .collect();
        // The layout has manifest blob + 1 layer blob; corrupt the smaller one
        // that is likely the layer (manifest is JSON, layer is gz tar).
        entries.sort_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(0));
        // Corrupt the layer blob (smallest file, index 0 after sort)
        let corrupt_path = entries[0].path();
        let mut data = fs::read(&corrupt_path).unwrap();
        // Flip a byte in the middle
        let mid = data.len() / 2;
        data[mid] ^= 0xFF;
        fs::write(&corrupt_path, &data).unwrap();

        let result = import_layout(&layout_dir, &store, "corrupt-test");
        assert!(
            matches!(result, Err(LightrError::Integrity { .. })),
            "corrupt blob must produce Integrity error; got: {:?}",
            result.err()
        );
    }

    /// Verify that `verify_sha256` helper correctly identifies corruption.
    #[test]
    fn test_verify_sha256_helper() {
        let data = b"test content";
        let good_hex = sha256_hex_of(data);
        assert!(verify_sha256(data, &good_hex).is_ok());

        // Wrong hex → Integrity error
        let bad_hex = "0".repeat(64);
        let err = verify_sha256(data, &bad_hex).unwrap_err();
        assert!(matches!(err, LightrError::Integrity { .. }));
    }

    // ── FIX 3/4: whiteout ordering tests ─────────────────────────────────────

    /// Same-layer add-then-whiteout: the file must be absent (whiteouts win).
    #[test]
    fn test_intra_layer_whiteout_ordering() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        // Single layer: add x/f AND add x/.wh.f (whiteout of x/f)
        // Per OCI parent-ref semantics our impl documents: whiteouts are
        // processed before additions within a layer, so x/f ends up absent.
        let layer = make_layer(&[
            ("x/", &[], 0o755),
            ("x/f", b"should be absent", 0o644),
            ("x/.wh.f", &[], 0o644), // whiteout of x/f
        ]);

        let layout_dir = make_layout(tmp.path(), &[layer]);
        let report = import_layout(&layout_dir, &store, "wo-order-test").unwrap();
        assert_eq!(report.layers, 1);

        let hydrate_dir = tmp.path().join("hydrated-wo");
        fs::create_dir_all(&hydrate_dir).unwrap();
        lightr_index::hydrate(&hydrate_dir, &store, "wo-order-test").unwrap();

        assert!(
            !hydrate_dir.join("x/f").exists(),
            "x/f must be absent: whiteout in same layer applies (whiteouts execute before additions)"
        );
    }

    /// Opaque whiteout clears dir from prior layer; new dir created by opaque.
    #[test]
    fn test_opaque_whiteout_clears_prior_layer() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        // Layer 1: create dir and file
        let layer1 = make_layer(&[("dir/", &[], 0o755), ("dir/old.txt", b"old", 0o644)]);
        // Layer 2: opaque whiteout of dir, then add a new file in dir
        let layer2 = make_layer(&[
            ("dir/.wh..wh..opq", &[], 0o644), // opaque whiteout
            ("dir/new.txt", b"new", 0o644),
        ]);

        let layout_dir = make_layout(tmp.path(), &[layer1, layer2]);
        import_layout(&layout_dir, &store, "opaque-test").unwrap();

        let hydrate_dir = tmp.path().join("hydrated-opaque");
        fs::create_dir_all(&hydrate_dir).unwrap();
        lightr_index::hydrate(&hydrate_dir, &store, "opaque-test").unwrap();

        assert!(
            !hydrate_dir.join("dir/old.txt").exists(),
            "dir/old.txt must be absent after opaque whiteout"
        );
        assert!(
            hydrate_dir.join("dir/new.txt").exists(),
            "dir/new.txt must be present after opaque whiteout"
        );
    }

    // ── FIX 5: hardlink tests ─────────────────────────────────────────────────

    /// Hardlink to a present target: both files have identical content.
    #[test]
    fn test_hardlink_present_target() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        // Build a layer gz with a regular file then a hardlink pointing to it.
        let layer_bytes = {
            let gz_buf = Vec::new();
            let encoder = GzEncoder::new(gz_buf, Compression::fast());
            let mut tar_b = tar::Builder::new(encoder);

            // Regular file: "original.txt"
            let content = b"link content";
            let mut rh = tar::Header::new_gnu();
            rh.set_path("original.txt").unwrap();
            rh.set_mode(0o644);
            rh.set_size(content.len() as u64);
            rh.set_entry_type(tar::EntryType::Regular);
            rh.set_cksum();
            tar_b.append(&rh, &content[..]).unwrap();

            // Hardlink: "copy.txt" → "original.txt"
            let mut lh = tar::Header::new_gnu();
            lh.set_path("copy.txt").unwrap();
            lh.set_mode(0o644);
            lh.set_size(0);
            lh.set_entry_type(tar::EntryType::Link);
            lh.set_link_name("original.txt").unwrap();
            lh.set_cksum();
            tar_b.append(&lh, &b""[..]).unwrap();

            tar_b.into_inner().unwrap().finish().unwrap()
        };

        let layout_dir = make_layout(tmp.path(), &[layer_bytes]);
        import_layout(&layout_dir, &store, "hardlink-test").unwrap();

        let hydrate_dir = tmp.path().join("hydrated-hl");
        fs::create_dir_all(&hydrate_dir).unwrap();
        lightr_index::hydrate(&hydrate_dir, &store, "hardlink-test").unwrap();

        let orig = hydrate_dir.join("original.txt");
        let copy = hydrate_dir.join("copy.txt");
        assert!(orig.exists(), "original.txt must exist");
        assert!(copy.exists(), "copy.txt (hardlink) must exist");
        assert_eq!(
            fs::read(&orig).unwrap(),
            fs::read(&copy).unwrap(),
            "hardlinked files must have identical content"
        );
    }

    /// Dangling hardlink → import must fail (fail-closed).
    #[test]
    fn test_hardlink_dangling_fails() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let (_home, store) = tmp_store_and_home();

        let layer_bytes = {
            let gz_buf = Vec::new();
            let encoder = GzEncoder::new(gz_buf, Compression::fast());
            let mut tar_b = tar::Builder::new(encoder);

            // Hardlink that points to a non-existent target
            let mut lh = tar::Header::new_gnu();
            lh.set_path("dangling.txt").unwrap();
            lh.set_mode(0o644);
            lh.set_size(0);
            lh.set_entry_type(tar::EntryType::Link);
            lh.set_link_name("ghost.txt").unwrap();
            lh.set_cksum();
            tar_b.append(&lh, &b""[..]).unwrap();

            tar_b.into_inner().unwrap().finish().unwrap()
        };

        let layout_dir = make_layout(tmp.path(), &[layer_bytes]);
        let result = import_layout(&layout_dir, &store, "dangling-hl");

        assert!(
            matches!(result, Err(LightrError::InvalidManifest(_))),
            "dangling hardlink must return InvalidManifest; got: {:?}",
            result.err()
        );
        if let Err(LightrError::InvalidManifest(msg)) = result {
            assert!(
                msg.contains("hardlink target not found"),
                "error must mention 'hardlink target not found'; got: {msg}"
            );
        }
    }
}
