//! Path-safety, SHA-256 helpers, TempDirGuard, and host-arch detection.

use lightr_core::{Digest, LightrError, Result};
use sha2::{Digest as Sha2Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

// ─────────────────────────────────────────────────────────────────────────────
// TempDir guard — cleans up on drop
// ─────────────────────────────────────────────────────────────────────────────

pub(super) struct TempDirGuard(pub(super) PathBuf);

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
pub(super) fn path_is_safe(p: &Path) -> bool {
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
pub(super) fn sha256_hex(digest: &str) -> Option<&str> {
    digest.strip_prefix("sha256:")
}

// ─────────────────────────────────────────────────────────────────────────────
// SHA-256 integrity helpers (FIX 1: REAL sha256 verification — close FAIL-OPEN)
// ─────────────────────────────────────────────────────────────────────────────

/// Compute the SHA-256 of `data` and return it as a lowercase hex string.
pub(super) fn sha256_hex_of(data: &[u8]) -> String {
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
pub(super) fn verify_sha256(data: &[u8], expected_hex: &str) -> Result<()> {
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
pub(super) fn hex_to_digest(hex: &str) -> Option<Digest> {
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

pub(super) fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Finalize a sha256 hasher into a lowercase hex string.
pub(super) fn hasher_to_hex(hasher: Sha256) -> String {
    let bytes = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in bytes.iter() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Multi-arch selection (WP-A-pull item 5)
// ─────────────────────────────────────────────────────────────────────────────

/// Extract `"<os>/<arch>"` from an OCI image config JSON's top-level `os` +
/// `architecture` fields (WP-IMG-01 platform retention at import, where the
/// manifest descriptor carries no platform). Returns `""` when the config is
/// unparsable or either field is missing — platform is best-effort metadata.
pub(super) fn platform_of_config(config_bytes: &[u8]) -> String {
    #[derive(serde::Deserialize)]
    struct Cfg {
        #[serde(default)]
        os: String,
        #[serde(default)]
        architecture: String,
    }
    match serde_json::from_slice::<Cfg>(config_bytes) {
        Ok(c) if !c.os.is_empty() && !c.architecture.is_empty() => {
            format!("{}/{}", c.os, c.architecture)
        }
        _ => String::new(),
    }
}

/// Map `std::env::consts::ARCH` → OCI architecture string.
pub(super) fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}
