//! Image reference parsing, manifest-list platform selection, docker token fetch.

use super::http::{read_response_bytes, retry_request, RegistryCreds};
use super::model::{OciDescriptor, TokenResponse};
use super::util::host_arch;
use lightr_core::{LightrError, Result};

// ─────────────────────────────────────────────────────────────────────────────
// Image reference parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse an image reference into `(registry, repo, tag)`.
///
/// FIX 6: reject empty or structurally invalid refs → `LightrError::InvalidRef`
/// (maps to exit 2 in the CLI). Validation rules:
///   - ref must be non-empty
///   - repo must be non-empty after stripping the registry prefix
///   - tag must be non-empty
///   - repo components must contain only `[a-z0-9._/-]` (OCI ref grammar)
pub(super) fn parse_image_ref(image: &str) -> Result<(String, String, String)> {
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

// ─────────────────────────────────────────────────────────────────────────────
// Multi-arch selection (WP-A-pull item 5)
// ─────────────────────────────────────────────────────────────────────────────

/// Pick a manifest descriptor from a manifest list:
///   1. `linux/<host-arch>`
///   2. `linux/amd64` fallback
///   3. Any `linux/*` entry fallback
///   4. Error listing available arches.
pub(super) fn pick_from_manifest_list(manifests: &[OciDescriptor]) -> Result<&OciDescriptor> {
    let arch = host_arch();

    // Collect linux entries for fallback reporting.
    let linux_entries: Vec<&OciDescriptor> = manifests
        .iter()
        .filter(|m| {
            m.platform
                .as_ref()
                .map(|p| p.os == "linux")
                .unwrap_or(false)
        })
        .collect();

    // 1. Exact match: linux/<host>.
    if let Some(m) = linux_entries.iter().find(|m| {
        m.platform
            .as_ref()
            .map(|p| p.architecture == arch)
            .unwrap_or(false)
    }) {
        return Ok(m);
    }

    // 2. Fallback to linux/amd64.
    if arch != "amd64" {
        if let Some(m) = linux_entries.iter().find(|m| {
            m.platform
                .as_ref()
                .map(|p| p.architecture == "amd64")
                .unwrap_or(false)
        }) {
            return Ok(m);
        }
    }

    // 3. Any linux entry.
    if let Some(m) = linux_entries.first() {
        return Ok(m);
    }

    // 4. Error: list what was available.
    let available: Vec<String> = manifests
        .iter()
        .filter_map(|m| {
            m.platform
                .as_ref()
                .map(|p| format!("{}/{}", p.os, p.architecture))
        })
        .collect();
    Err(LightrError::InvalidManifest(format!(
        "manifest list has no linux entry; available: [{}]",
        available.join(", ")
    )))
}

// ─────────────────────────────────────────────────────────────────────────────
// Docker Hub token fetch
// ─────────────────────────────────────────────────────────────────────────────

pub(super) fn fetch_docker_token(
    agent: &ureq::Agent,
    repo: &str,
    creds: Option<&RegistryCreds>,
    scope: &str,
) -> Result<String> {
    let url = format!(
        "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:{scope}"
    );

    let resp = retry_request(
        || {
            let mut req = agent.get(&url);
            // Use Basic auth on the token endpoint if we have credentials.
            // NEVER log the auth string.
            if let Some(c) = creds {
                req = req.set("Authorization", &format!("Basic {}", c.b64));
            }
            req.call()
        },
        repo,
    )?;

    let body = read_response_bytes(resp)?;
    let token_resp: TokenResponse = serde_json::from_slice(&body)
        .map_err(|e| LightrError::InvalidManifest(format!("token response parse error: {e}")))?;
    Ok(token_resp.token)
}
