use super::consts::REF_KEY_DOMAIN;
use super::digest::Digest;
use super::error::{LightrError, Result};

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
