//! Binary codec for the stat-index: format, constants, IndexEntry, Index,
//! and the path-helper pair (fsync_dir, index_dir, index_path_for).
//!
//! Format: magic "LIX1" · u32 version=1 · u64 saved_at_unix_ns · u32 count
//! · records path-sorted:
//!     u8 kind · u32 mode · u64 size · u64 mtime_ns · u64 ino · 32B digest
//!     · u16 path_len · path (UTF-8)
//! All LE.

use lightr_core::{Digest, LightrError, Result};
#[cfg(unix)]
use std::fs::File;
use std::{
    collections::HashMap,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub(crate) const INDEX_MAGIC: &[u8; 4] = b"LIX1";
pub(crate) const INDEX_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// fsync helper
// ---------------------------------------------------------------------------

/// fsync the parent directory so the rename (directory entry change) is
/// crash-durable on macOS/Linux. Mirrors the same helper in lightr-store.
///
/// On Windows: NTFS has no portable directory fsync API. This is a documented
/// no-op on Windows — data durability relies on FlushFileBuffers called on the
/// file itself before rename (done in save_for). The directory entry update may
/// not be crash-synced; a crash between rename and a hypothetical dir-fsync
/// means re-scan on next open, not corruption (the index is a cache).
pub(crate) fn fsync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let f = File::open(dir)?;
        f.sync_all()?;
    }
    // Windows: documented no-op — see function doc above.
    #[cfg(windows)]
    let _ = dir;
    Ok(())
}

// ---------------------------------------------------------------------------
// Index file path helpers
// ---------------------------------------------------------------------------

/// Returns `$LIGHTR_HOME/index/<blake3(canonicalized-root-abs-path)-hex>`.
/// Mirrors `lightr_store::Store::default_root` but for the index dir.
pub(crate) fn index_dir() -> PathBuf {
    if let Ok(h) = std::env::var("LIGHTR_HOME") {
        PathBuf::from(h).join("index")
    } else {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));
        home.join(".lightr").join("index")
    }
}

pub(crate) fn index_path_for(root: &Path) -> Result<PathBuf> {
    let canonical = root.canonicalize().map_err(LightrError::Io)?;
    let abs = canonical.to_string_lossy();
    let digest = Digest::of_bytes(abs.as_bytes());
    Ok(index_dir().join(digest.to_hex()))
}

// ---------------------------------------------------------------------------
// IndexEntry
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct IndexEntry {
    pub(crate) kind: u8, // 0=File, 1=Symlink, 2=Dir
    pub(crate) mode: u32,
    pub(crate) size: u64,
    pub(crate) mtime_ns: u64,
    pub(crate) ino: u64,
    pub(crate) digest: Digest,
    pub(crate) path: String,
}

// ---------------------------------------------------------------------------
// Index
// ---------------------------------------------------------------------------

pub struct Index {
    pub(crate) saved_at_ns: u64,
    pub(crate) entries: Vec<IndexEntry>,
    /// Quick lookup: path → position in entries
    pub(crate) by_path: HashMap<String, usize>,
}

impl Index {
    pub(crate) fn empty() -> Self {
        Index {
            saved_at_ns: 0,
            entries: Vec::new(),
            by_path: HashMap::new(),
        }
    }

    /// Look up an entry by relative path.
    pub(crate) fn get(&self, path: &str) -> Option<&IndexEntry> {
        self.by_path.get(path).map(|&i| &self.entries[i])
    }

    /// Insert or replace an entry.
    pub(crate) fn upsert(&mut self, entry: IndexEntry) {
        if let Some(&i) = self.by_path.get(&entry.path) {
            self.entries[i] = entry;
        } else {
            let i = self.entries.len();
            self.by_path.insert(entry.path.clone(), i);
            self.entries.push(entry);
        }
    }

    pub fn load_for(root: &Path) -> Result<Self> {
        let path = match index_path_for(root) {
            Ok(p) => p,
            Err(_) => return Ok(Self::empty()),
        };

        let data = match std::fs::read(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(_) => return Ok(Self::empty()), // corrupt: treat as empty
            Ok(d) => d,
        };

        // Decode — any error ⇒ empty (index is a cache, never an error source)
        match Self::decode(&data) {
            Ok(idx) => Ok(idx),
            Err(_) => Ok(Self::empty()),
        }
    }

    fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 4 + 4 + 8 + 4 {
            return Err(LightrError::InvalidManifest("index too short".into()));
        }
        if &data[..4] != INDEX_MAGIC {
            return Err(LightrError::InvalidManifest("bad index magic".into()));
        }
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        if version != INDEX_VERSION {
            return Err(LightrError::InvalidManifest("unknown index version".into()));
        }
        let saved_at_ns = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let count = u32::from_le_bytes(data[16..20].try_into().unwrap()) as usize;

        let mut pos = 20usize;
        let mut entries = Vec::with_capacity(count);

        for _ in 0..count {
            if pos + 1 + 4 + 8 + 8 + 8 + 32 + 2 > data.len() {
                return Err(LightrError::InvalidManifest("index truncated".into()));
            }
            let kind = data[pos];
            pos += 1;
            let mode = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let size = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let mtime_ns = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let ino = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let mut digest_bytes = [0u8; 32];
            digest_bytes.copy_from_slice(&data[pos..pos + 32]);
            let digest = Digest(digest_bytes);
            pos += 32;
            let path_len =
                u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
            pos += 2;
            if pos + path_len > data.len() {
                return Err(LightrError::InvalidManifest("index path truncated".into()));
            }
            let path = std::str::from_utf8(&data[pos..pos + path_len])
                .map_err(|_| LightrError::InvalidManifest("non-utf8 path".into()))?
                .to_string();
            pos += path_len;
            entries.push(IndexEntry {
                kind,
                mode,
                size,
                mtime_ns,
                ino,
                digest,
                path,
            });
        }

        let mut by_path = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            by_path.insert(e.path.clone(), i);
        }

        Ok(Index {
            saved_at_ns,
            entries,
            by_path,
        })
    }

    pub fn save_for(&self, root: &Path) -> Result<()> {
        let index_path = index_path_for(root)?;
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent).map_err(LightrError::Io)?;
        }

        // Encode
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(INDEX_MAGIC);
        buf.extend_from_slice(&INDEX_VERSION.to_le_bytes());

        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        buf.extend_from_slice(&now_ns.to_le_bytes());

        // Sort entries by path for the file
        let mut sorted: Vec<&IndexEntry> = self.entries.iter().collect();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));

        buf.extend_from_slice(&(sorted.len() as u32).to_le_bytes());

        for e in &sorted {
            buf.push(e.kind);
            buf.extend_from_slice(&e.mode.to_le_bytes());
            buf.extend_from_slice(&e.size.to_le_bytes());
            buf.extend_from_slice(&e.mtime_ns.to_le_bytes());
            buf.extend_from_slice(&e.ino.to_le_bytes());
            buf.extend_from_slice(&e.digest.0);
            let path_bytes = e.path.as_bytes();
            buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
            buf.extend_from_slice(path_bytes);
        }

        // Atomic write: write to a tmp path in same dir, then rename.
        let dir = index_path.parent().unwrap_or(Path::new("."));
        static TMP_COUNTER: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let tmp_path = dir.join(format!(
            ".lightr-index-tmp-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        {
            let mut f = std::fs::File::create(&tmp_path).map_err(LightrError::Io)?;
            f.write_all(&buf).map_err(LightrError::Io)?;
            // fsync data before rename for crash durability.
            // On Windows, File::sync_all() maps to FlushFileBuffers internally.
            // WIN-PATH: this is a best-effort sync before rename on Windows.
            f.sync_all().map_err(LightrError::Io)?;
            // f dropped (closed) here before rename
        }
        std::fs::rename(&tmp_path, &index_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            LightrError::Io(e)
        })?;
        // fsync parent dir so the rename (directory entry update) is crash-durable.
        fsync_dir(dir).map_err(LightrError::Io)?;

        Ok(())
    }
}
