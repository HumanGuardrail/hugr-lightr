use super::consts::MANIFEST_MAGIC;
use super::digest::Digest;
use super::error::{LightrError, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    File {
        path: String,
        mode: u32,
        size: u64,
        digest: Digest,
    },
    Symlink {
        path: String,
        target: String,
    },
    Dir {
        path: String,
    },
}

impl Entry {
    pub fn path(&self) -> &str {
        match self {
            Entry::File { path, .. } | Entry::Symlink { path, .. } | Entry::Dir { path } => path,
        }
    }
}

// LMF1 entry kind tags
pub(super) const KIND_FILE: u8 = 0;
pub(super) const KIND_SYMLINK: u8 = 1;
pub(super) const KIND_DIR: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub total_size: u64,
    pub entries: Vec<Entry>,
}

impl Manifest {
    /// Encode to LMF1 binary format (little-endian).
    /// Asserts (debug) that entries are path-sorted.
    pub fn encode(&self) -> Vec<u8> {
        debug_assert!(
            self.entries.windows(2).all(|w| w[0].path() <= w[1].path()),
            "entries must be path-sorted before encoding"
        );

        let mut buf: Vec<u8> = Vec::new();

        // magic "LMF1"
        buf.extend_from_slice(MANIFEST_MAGIC);
        // u32 version
        buf.extend_from_slice(&self.version.to_le_bytes());
        // u64 total_size
        buf.extend_from_slice(&self.total_size.to_le_bytes());
        // u32 entry_count
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());

        for entry in &self.entries {
            match entry {
                Entry::File {
                    path,
                    mode,
                    size,
                    digest,
                } => {
                    buf.push(KIND_FILE);
                    buf.extend_from_slice(&mode.to_le_bytes());
                    buf.extend_from_slice(&size.to_le_bytes());
                    buf.extend_from_slice(&digest.0);
                    let path_bytes = path.as_bytes();
                    buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
                    buf.extend_from_slice(path_bytes);
                }
                Entry::Symlink { path, target } => {
                    buf.push(KIND_SYMLINK);
                    buf.extend_from_slice(&0u32.to_le_bytes()); // mode = 0
                    buf.extend_from_slice(&0u64.to_le_bytes()); // size = 0
                    buf.extend_from_slice(&[0u8; 32]); // digest zeroed
                    let path_bytes = path.as_bytes();
                    buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
                    buf.extend_from_slice(path_bytes);
                    let target_bytes = target.as_bytes();
                    buf.extend_from_slice(&(target_bytes.len() as u16).to_le_bytes());
                    buf.extend_from_slice(target_bytes);
                }
                Entry::Dir { path } => {
                    buf.push(KIND_DIR);
                    buf.extend_from_slice(&0u32.to_le_bytes()); // mode = 0
                    buf.extend_from_slice(&0u64.to_le_bytes()); // size = 0
                    buf.extend_from_slice(&[0u8; 32]); // digest zeroed
                    let path_bytes = path.as_bytes();
                    buf.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
                    buf.extend_from_slice(path_bytes);
                }
            }
        }

        buf
    }

    /// Decode from LMF1 binary format. Returns InvalidManifest on any
    /// truncation, bad magic, or unsupported version.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut cur = 0usize;

        macro_rules! need {
            ($n:expr) => {{
                let n = $n;
                if cur + n > bytes.len() {
                    return Err(LightrError::InvalidManifest(
                        "truncated manifest".to_string(),
                    ));
                }
                let slice = &bytes[cur..cur + n];
                cur += n;
                slice
            }};
        }

        // magic
        let magic = need!(4);
        if magic != MANIFEST_MAGIC {
            return Err(LightrError::InvalidManifest(
                "bad magic — expected LMF1".to_string(),
            ));
        }

        // version
        let version = u32::from_le_bytes(need!(4).try_into().unwrap());
        if version != 1 {
            return Err(LightrError::InvalidManifest(format!(
                "unsupported manifest version: {version}"
            )));
        }

        // total_size
        let total_size = u64::from_le_bytes(need!(8).try_into().unwrap());

        // entry_count
        let entry_count = u32::from_le_bytes(need!(4).try_into().unwrap()) as usize;

        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let kind = need!(1)[0];
            let mode = u32::from_le_bytes(need!(4).try_into().unwrap());
            let size = u64::from_le_bytes(need!(8).try_into().unwrap());
            let digest_bytes: [u8; 32] = need!(32).try_into().unwrap();
            let path_len = u16::from_le_bytes(need!(2).try_into().unwrap()) as usize;
            let path_bytes = need!(path_len);
            let path = std::str::from_utf8(path_bytes)
                .map_err(|_| LightrError::InvalidManifest("non-UTF8 path in manifest".to_string()))?
                .to_string();

            let entry = match kind {
                KIND_FILE => Entry::File {
                    path,
                    mode,
                    size,
                    digest: Digest(digest_bytes),
                },
                KIND_SYMLINK => {
                    let target_len = u16::from_le_bytes(need!(2).try_into().unwrap()) as usize;
                    let target_bytes = need!(target_len);
                    let target = std::str::from_utf8(target_bytes)
                        .map_err(|_| {
                            LightrError::InvalidManifest(
                                "non-UTF8 symlink target in manifest".to_string(),
                            )
                        })?
                        .to_string();
                    Entry::Symlink { path, target }
                }
                KIND_DIR => Entry::Dir { path },
                other => {
                    return Err(LightrError::InvalidManifest(format!(
                        "unknown entry kind: {other}"
                    )))
                }
            };
            entries.push(entry);
        }

        if cur != bytes.len() {
            return Err(LightrError::InvalidManifest(
                "trailing bytes in manifest".to_string(),
            ));
        }

        Ok(Manifest {
            version,
            total_size,
            entries,
        })
    }

    pub fn digest(&self) -> Digest {
        Digest::of_bytes(&self.encode())
    }
}
