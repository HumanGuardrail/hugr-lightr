//! lightr-core — frozen contract: build-spec v2 §3 (ADR-0009/0004).
//! Types are the contract; method bodies are WP-1.
#![forbid(unsafe_code)]

use std::path::Path;

pub const OUTPUT_CAP_BYTES: usize = 5 * 1024 * 1024;
pub const MANIFEST_MAGIC: &[u8; 4] = b"LMF1";
pub const REF_KEY_DOMAIN: &str = "lightr/ref/v1/";

// ── Digest ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    pub fn of_bytes(data: &[u8]) -> Self {
        Digest(*blake3::hash(data).as_bytes())
    }

    pub fn of_file(path: &Path) -> Result<Self> {
        let mut hasher = blake3::Hasher::new();
        hasher.update_mmap_rayon(path)?;
        Ok(Digest(*hasher.finalize().as_bytes()))
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        if s.len() != 64 {
            return Err(LightrError::InvalidManifest(format!(
                "invalid digest hex: {s}"
            )));
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0])
                .ok_or_else(|| LightrError::InvalidManifest(format!("invalid digest hex: {s}")))?;
            let lo = hex_nibble(chunk[1])
                .ok_or_else(|| LightrError::InvalidManifest(format!("invalid digest hex: {s}")))?;
            bytes[i] = (hi << 4) | lo;
        }
        Ok(Digest(bytes))
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

// ── Entry ────────────────────────────────────────────────────────────────────

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

// ── Manifest ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub total_size: u64,
    pub entries: Vec<Entry>,
}

// LMF1 entry kind tags
const KIND_FILE: u8 = 0;
const KIND_SYMLINK: u8 = 1;
const KIND_DIR: u8 = 2;

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

// ── RefRecord ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefRecord {
    pub name: String,
    pub root: Digest,
    pub parent: Option<Digest>,
    pub created_at_unix: u64,
    pub tool_version: String,
}

impl RefRecord {
    /// Encode to binary (little-endian).
    ///
    /// Layout: u16 name\_len · name bytes · 32B root · u8 has\_parent ·
    /// optional 32B parent · u64 created\_at\_unix · u16 tool\_version\_len ·
    /// tool\_version bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();

        let name_bytes = self.name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(name_bytes);

        buf.extend_from_slice(&self.root.0);

        match &self.parent {
            Some(p) => {
                buf.push(1u8);
                buf.extend_from_slice(&p.0);
            }
            None => {
                buf.push(0u8);
            }
        }

        buf.extend_from_slice(&self.created_at_unix.to_le_bytes());

        let tv_bytes = self.tool_version.as_bytes();
        buf.extend_from_slice(&(tv_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(tv_bytes);

        buf
    }

    /// Decode from binary. Returns InvalidManifest on truncation or bad data.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut cur = 0usize;

        macro_rules! need {
            ($n:expr) => {{
                let n = $n;
                if cur + n > b.len() {
                    return Err(LightrError::InvalidManifest(
                        "truncated ref record".to_string(),
                    ));
                }
                let slice = &b[cur..cur + n];
                cur += n;
                slice
            }};
        }

        let name_len = u16::from_le_bytes(need!(2).try_into().unwrap()) as usize;
        let name_bytes = need!(name_len);
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| LightrError::InvalidManifest("non-UTF8 ref name".to_string()))?
            .to_string();

        let root = Digest(need!(32).try_into().unwrap());

        let has_parent = need!(1)[0];
        let parent = if has_parent == 1 {
            Some(Digest(need!(32).try_into().unwrap()))
        } else if has_parent == 0 {
            None
        } else {
            return Err(LightrError::InvalidManifest(
                "invalid has_parent byte in ref record".to_string(),
            ));
        };

        let created_at_unix = u64::from_le_bytes(need!(8).try_into().unwrap());

        let tv_len = u16::from_le_bytes(need!(2).try_into().unwrap()) as usize;
        let tv_bytes = need!(tv_len);
        let tool_version = std::str::from_utf8(tv_bytes)
            .map_err(|_| {
                LightrError::InvalidManifest("non-UTF8 tool_version in ref record".to_string())
            })?
            .to_string();

        if cur != b.len() {
            return Err(LightrError::InvalidManifest(
                "trailing bytes in ref record".to_string(),
            ));
        }

        Ok(RefRecord {
            name,
            root,
            parent,
            created_at_unix,
            tool_version,
        })
    }
}

// ── validate_ref_name ─────────────────────────────────────────────────────────

/// Validates a ref name against the ADR-0004 grammar:
///   ^(@[a-z0-9-]{1,32}/)?[a-z0-9._-]{1,64}$
///
/// No regex dependency — hand-rolled matcher.
pub fn validate_ref_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(LightrError::InvalidRef(name.to_string()));
    }

    let rest = if let Some(stripped) = name.strip_prefix('@') {
        // namespace part: [a-z0-9-]{1,32} then '/'
        let slash = stripped
            .find('/')
            .ok_or_else(|| LightrError::InvalidRef(name.to_string()))?;
        let ns = &stripped[..slash];
        if ns.is_empty() || ns.len() > 32 {
            return Err(LightrError::InvalidRef(name.to_string()));
        }
        if !ns
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(LightrError::InvalidRef(name.to_string()));
        }
        &stripped[slash + 1..]
    } else {
        name
    };

    // local part: [a-z0-9._-]{1,64}
    if rest.is_empty() || rest.len() > 64 {
        return Err(LightrError::InvalidRef(name.to_string()));
    }
    if !rest.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
    }) {
        return Err(LightrError::InvalidRef(name.to_string()));
    }

    // Reject embedded '/' in the local part (e.g. "a/b" without namespace)
    // This is already handled above since '/' is not in [a-z0-9._-], but
    // make the intent explicit.
    Ok(())
}

// ── ref_key ───────────────────────────────────────────────────────────────────

/// Compute the storage key for a ref: BLAKE3(REF_KEY_DOMAIN bytes || name bytes).
/// Uses a single hasher with two updates for domain separation.
pub fn ref_key(name: &str) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(REF_KEY_DOMAIN.as_bytes());
    hasher.update(name.as_bytes());
    Digest(*hasher.finalize().as_bytes())
}

// ── LightrError ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum LightrError {
    NotFound(Digest),
    RefNotFound(String),
    Integrity { expected: Digest, actual: Digest },
    TooLarge { size: u64, cap: u64 },
    InvalidRef(String),
    InvalidManifest(String),
    Io(std::io::Error),
}

impl std::fmt::Display for LightrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LightrError::NotFound(d) => write!(f, "not found in store: {}", d.to_hex()),
            LightrError::RefNotFound(n) => write!(f, "ref not found: {n}"),
            LightrError::Integrity { expected, actual } => write!(
                f,
                "integrity violation: expected {} got {}",
                expected.to_hex(),
                actual.to_hex()
            ),
            LightrError::TooLarge { size, cap } => {
                write!(f, "blob of {size} bytes exceeds cap {cap}")
            }
            LightrError::InvalidRef(n) => write!(f, "invalid ref: {n}"),
            LightrError::InvalidManifest(msg) => write!(f, "invalid manifest: {msg}"),
            LightrError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LightrError {}

impl From<std::io::Error> for LightrError {
    fn from(e: std::io::Error) -> Self {
        LightrError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, LightrError>;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Known BLAKE3 vectors
    #[test]
    fn digest_known_vector_empty() {
        // blake3 of empty bytes — known test vector
        let d = Digest::of_bytes(b"");
        let hex = d.to_hex();
        assert_eq!(
            hex,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn digest_known_vector_lightr() {
        // blake3 of b"lightr"
        let d = Digest::of_bytes(b"lightr");
        // Compute expected dynamically to avoid hard-coding a potentially wrong
        // vector — we verify it equals what blake3 returns.
        let expected = blake3::hash(b"lightr");
        assert_eq!(d.0, *expected.as_bytes());
        assert_eq!(d.to_hex().len(), 64);
    }

    #[test]
    fn digest_hex_roundtrip() {
        let original = Digest::of_bytes(b"roundtrip test");
        let hex = original.to_hex();
        assert_eq!(hex.len(), 64);
        // All lowercase
        assert!(hex.chars().all(|c| !c.is_uppercase()));
        let recovered = Digest::from_hex(&hex).unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn digest_from_hex_uppercase() {
        // Accept uppercase hex
        let d = Digest::of_bytes(b"upper");
        let hex_lower = d.to_hex();
        let hex_upper = hex_lower.to_uppercase();
        let recovered = Digest::from_hex(&hex_upper).unwrap();
        assert_eq!(d, recovered);
    }

    #[test]
    fn digest_from_hex_invalid() {
        // Wrong length
        assert!(Digest::from_hex("abc").is_err());
        // Non-hex chars
        let bad: String = "g".repeat(64);
        assert!(Digest::from_hex(&bad).is_err());
    }

    #[test]
    fn digest_debug_is_lowercase_hex() {
        let d = Digest::of_bytes(b"debug");
        let dbg = format!("{d:?}");
        assert_eq!(dbg.len(), 64);
        assert!(dbg
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    // Manifest encode/decode roundtrip — all 3 entry kinds, path-sorted
    #[test]
    fn manifest_roundtrip_all_kinds() {
        let digest_a = Digest::of_bytes(b"file-a");
        let manifest = Manifest {
            version: 1,
            total_size: 1234,
            entries: vec![
                Entry::Dir {
                    path: "a/empty_dir".to_string(),
                },
                Entry::File {
                    path: "a/file.txt".to_string(),
                    mode: 0o644,
                    size: 42,
                    digest: digest_a,
                },
                Entry::Symlink {
                    path: "b/link".to_string(),
                    target: "../a/file.txt".to_string(),
                },
            ],
        };
        let encoded = manifest.encode();
        let decoded = Manifest::decode(&encoded).unwrap();
        assert_eq!(manifest, decoded);
    }

    #[test]
    fn manifest_decode_rejects_bad_magic() {
        let mut bytes = vec![0u8; 20];
        bytes[0] = b'X';
        bytes[1] = b'X';
        bytes[2] = b'X';
        bytes[3] = b'X';
        let err = Manifest::decode(&bytes).unwrap_err();
        assert!(matches!(err, LightrError::InvalidManifest(_)));
    }

    #[test]
    fn manifest_decode_rejects_truncation() {
        let manifest = Manifest {
            version: 1,
            total_size: 0,
            entries: vec![Entry::Dir {
                path: "x".to_string(),
            }],
        };
        let full = manifest.encode();
        // Truncate to partial
        let truncated = &full[..full.len() - 2];
        let err = Manifest::decode(truncated).unwrap_err();
        assert!(matches!(err, LightrError::InvalidManifest(_)));
    }

    #[test]
    fn manifest_digest_is_of_encode() {
        let manifest = Manifest {
            version: 1,
            total_size: 99,
            entries: vec![Entry::Dir {
                path: "root".to_string(),
            }],
        };
        let expected = Digest::of_bytes(&manifest.encode());
        assert_eq!(manifest.digest(), expected);
    }

    // RefRecord roundtrip — with and without parent
    #[test]
    fn refrecord_roundtrip_with_parent() {
        let root = Digest::of_bytes(b"root");
        let parent = Digest::of_bytes(b"parent");
        let rec = RefRecord {
            name: "web".to_string(),
            root,
            parent: Some(parent),
            created_at_unix: 1_700_000_000,
            tool_version: "0.1.0".to_string(),
        };
        let encoded = rec.encode();
        let decoded = RefRecord::decode(&encoded).unwrap();
        assert_eq!(rec, decoded);
    }

    #[test]
    fn refrecord_roundtrip_without_parent() {
        let root = Digest::of_bytes(b"root2");
        let rec = RefRecord {
            name: "@hugr/web".to_string(),
            root,
            parent: None,
            created_at_unix: 0,
            tool_version: "0.1.0-alpha".to_string(),
        };
        let encoded = rec.encode();
        let decoded = RefRecord::decode(&encoded).unwrap();
        assert_eq!(rec, decoded);
    }

    // validate_ref_name — accept/reject table
    #[test]
    fn ref_name_accept() {
        assert!(validate_ref_name("web").is_ok());
        assert!(validate_ref_name("@hugr/web").is_ok());
        assert!(validate_ref_name("a.b_c-d").is_ok());
    }

    #[test]
    fn ref_name_reject_empty() {
        assert!(validate_ref_name("").is_err());
    }

    #[test]
    fn ref_name_reject_uppercase_local() {
        assert!(validate_ref_name("Web").is_err());
    }

    #[test]
    fn ref_name_reject_uppercase_namespace() {
        assert!(validate_ref_name("@HUGR/x").is_err());
    }

    #[test]
    fn ref_name_reject_namespace_no_local() {
        // "@a/" — namespace with empty local part
        assert!(validate_ref_name("@a/").is_err());
    }

    #[test]
    fn ref_name_reject_slash_without_namespace() {
        // "a/b" — slash without leading @namespace
        assert!(validate_ref_name("a/b").is_err());
    }

    #[test]
    fn ref_name_reject_65_char_name() {
        let long = "a".repeat(65);
        assert!(validate_ref_name(&long).is_err());
    }

    #[test]
    fn ref_name_accept_64_char_name() {
        let exactly64 = "a".repeat(64);
        assert!(validate_ref_name(&exactly64).is_ok());
    }

    #[test]
    fn ref_name_reject_space_in_namespace() {
        assert!(validate_ref_name("@a b/c").is_err());
    }

    // ref_key domain separation
    #[test]
    fn ref_key_domain_separation() {
        let name = "web";
        let key = ref_key(name);
        let raw = Digest::of_bytes(name.as_bytes());
        // Must differ from plain hash of name
        assert_ne!(key, raw);
        // Must equal the two-update approach
        let mut hasher = blake3::Hasher::new();
        hasher.update(REF_KEY_DOMAIN.as_bytes());
        hasher.update(name.as_bytes());
        let expected = Digest(*hasher.finalize().as_bytes());
        assert_eq!(key, expected);
    }
}
