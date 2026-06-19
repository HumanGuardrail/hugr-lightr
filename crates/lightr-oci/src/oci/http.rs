//! HTTP agent, registry credentials, retry, streaming blob download, registry_scheme.

use super::util::hex_to_digest;
use lightr_core::{Digest, LightrError, Result};
use sha2::{Digest as Sha2Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};

// ─────────────────────────────────────────────────────────────────────────────
// ureq agent with explicit timeouts (ureq v2: timeout_connect on AgentBuilder)
// ─────────────────────────────────────────────────────────────────────────────

pub(super) fn net_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build()
}

// ─────────────────────────────────────────────────────────────────────────────
// Private-registry auth (WP-A-pull item 1)
// ─────────────────────────────────────────────────────────────────────────────

/// Credentials for a registry: base64-encoded "user:pass".
/// Returned value is ready to use as `Basic <value>` in an Authorization header.
/// NEVER logs or stores the raw value beyond the returned String lifetime.
pub(super) struct RegistryCreds {
    /// Base64-encoded "user:pass" — use as `Basic <b64>`.
    pub(super) b64: String,
}

/// Look up credentials for `registry` in Docker's config.json (or the
/// `LIGHTR_REGISTRY_AUTH` env override).
///
/// Priority:
///   1. `LIGHTR_REGISTRY_AUTH` env var (base64 user:pass) — always wins.
///   2. `~/.docker/config.json` → `auths.<registry>.auth` field.
///   3. `$DOCKER_CONFIG/config.json` if `DOCKER_CONFIG` is set.
///
/// Returns `None` (anonymous) if the file is missing or has no entry.
///
/// Never panics on I/O or parse errors — just returns `None`.
pub(super) fn read_creds_for_registry(registry: &str) -> Option<RegistryCreds> {
    // 1. Env override wins.
    if let Ok(val) = std::env::var("LIGHTR_REGISTRY_AUTH") {
        let trimmed = val.trim().to_string();
        if !trimmed.is_empty() {
            return Some(RegistryCreds { b64: trimmed });
        }
    }

    // 2. Locate config.json.
    let config_path: std::path::PathBuf = if let Ok(dc) = std::env::var("DOCKER_CONFIG") {
        std::path::PathBuf::from(dc).join("config.json")
    } else {
        let home = std::env::var("HOME").ok()?;
        std::path::PathBuf::from(home)
            .join(".docker")
            .join("config.json")
    };

    parse_docker_config_for_registry(&config_path, registry)
}

/// Parse a docker config.json file at `path` and extract credentials for `registry`.
/// Separated from `read_creds_for_registry` so tests can call it without mutating env.
pub(super) fn parse_docker_config_for_registry(
    config_path: &Path,
    registry: &str,
) -> Option<RegistryCreds> {
    use serde::Deserialize;
    let raw = fs::read(config_path).ok()?;

    // Parse: {"auths": {"<registry>": {"auth": "<b64>"}}}
    #[derive(Deserialize)]
    struct DockerAuth {
        #[serde(default)]
        auth: String,
    }
    #[derive(Deserialize)]
    struct DockerConfig {
        #[serde(default)]
        auths: std::collections::HashMap<String, DockerAuth>,
    }

    let cfg: DockerConfig = serde_json::from_slice(&raw).ok()?;
    let entry = cfg.auths.get(registry)?;
    if entry.auth.is_empty() {
        return None;
    }
    Some(RegistryCreds {
        b64: entry.auth.clone(),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// HTTP status → typed errors (WP-A-pull item 4)
// ─────────────────────────────────────────────────────────────────────────────

/// Map a ureq error to `LightrError`.
/// - `ureq::Error::Status(code, _)` → Registry with typed message.
/// - `ureq::Error::Transport(_)`    → Io.
pub(super) fn map_ureq_error(e: ureq::Error, repo_or_ref: &str) -> LightrError {
    match e {
        ureq::Error::Status(401, _) => LightrError::Registry {
            status: 401,
            msg: format!("authentication required / forbidden for {repo_or_ref}"),
        },
        ureq::Error::Status(403, _) => LightrError::Registry {
            status: 403,
            msg: format!("authentication required / forbidden for {repo_or_ref}"),
        },
        ureq::Error::Status(404, _) => LightrError::Registry {
            status: 404,
            msg: format!("image or blob not found: {repo_or_ref}"),
        },
        ureq::Error::Status(429, _) => LightrError::Registry {
            status: 429,
            msg: "rate limited".to_string(),
        },
        ureq::Error::Status(code, _) if code >= 500 => LightrError::Registry {
            status: code,
            msg: format!("server error from registry for {repo_or_ref}"),
        },
        ureq::Error::Status(code, _) => LightrError::Registry {
            status: code,
            msg: format!("unexpected HTTP {code} for {repo_or_ref}"),
        },
        ureq::Error::Transport(t) => LightrError::Io(io::Error::other(t.to_string())),
    }
}

/// Extract the HTTP status code from a ureq::Error (Status variant only).
pub(super) fn ureq_status(e: &ureq::Error) -> Option<u16> {
    match e {
        ureq::Error::Status(code, _) => Some(*code),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Retry + backoff (WP-A-pull item 2)
// ─────────────────────────────────────────────────────────────────────────────

/// Retry a request closure up to 4 times on HTTP 429 or 5xx.
/// Exponential backoff: 200 ms, 400 ms, 800 ms, 1600 ms.
/// Honors `Retry-After` (seconds) header when present on 429/5xx.
/// 4xx responses except 429 are returned immediately (no retry).
///
/// `repo_or_ref` is used for error messages only.
///
/// The `result_large_err` allow is necessary because `ureq::Error` is a
/// large enum that we cannot control; boxing it here would require threading
/// `Box<ureq::Error>` through all callers.
#[allow(clippy::result_large_err)]
pub(super) fn retry_request<F>(f: F, repo_or_ref: &str) -> Result<ureq::Response>
where
    F: Fn() -> std::result::Result<ureq::Response, ureq::Error>,
{
    const MAX_RETRIES: u32 = 4;
    let mut delay_ms: u64 = 200;
    let mut last_err: Option<ureq::Error> = None;

    for attempt in 0..=MAX_RETRIES {
        match f() {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let maybe_status = ureq_status(&e);
                let should_retry = matches!(maybe_status, Some(429) | Some(500..=599));

                if !should_retry || attempt == MAX_RETRIES {
                    return Err(map_ureq_error(e, repo_or_ref));
                }

                // Honor Retry-After header on 429/5xx.
                let wait_ms = if let ureq::Error::Status(_, ref resp) = e {
                    resp.header("Retry-After")
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|secs| secs.saturating_mul(1000))
                        .unwrap_or(delay_ms)
                } else {
                    delay_ms
                };

                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                delay_ms = (delay_ms * 2).min(1600);
            }
        }
    }

    // last_err is always Some here (we only reach this if MAX_RETRIES attempts failed).
    Err(match last_err {
        Some(e) => map_ureq_error(e, repo_or_ref),
        None => LightrError::Registry {
            status: 0,
            msg: "retry logic exhausted".to_string(),
        },
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Streaming blob download with sha256 (WP-A-pull item 3)
// ─────────────────────────────────────────────────────────────────────────────

/// Download a blob from `url` into `dest_path`, computing sha256 **streaming**
/// over the same bytes (never materializes the full blob in RAM).
///
/// If `expected_hex` is `Some`, verifies the digest after download.
/// On mismatch → `LightrError::Integrity` (fail-closed).
pub(super) fn stream_blob_to_file(
    agent: &ureq::Agent,
    url: &str,
    auth_header: Option<&str>,
    dest_path: &Path,
    expected_hex: Option<&str>,
    repo_or_ref: &str,
) -> Result<()> {
    let resp = retry_request(
        || {
            let mut req = agent.get(url);
            if let Some(h) = auth_header {
                req = req.set("Authorization", h);
            }
            req.call()
        },
        repo_or_ref,
    )?;

    let mut reader = resp.into_reader();
    let mut file = fs::File::create(dest_path).map_err(LightrError::Io)?;
    let mut hasher = Sha256::new();

    // 64 KiB copy buffer.
    let mut buf = vec![0u8; 65536];
    loop {
        let n = reader.read(&mut buf).map_err(LightrError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(LightrError::Io)?;
    }
    file.flush().map_err(LightrError::Io)?;
    drop(file);

    if let Some(expected) = expected_hex {
        let actual_bytes = hasher.finalize();
        let mut actual_hex_str = String::with_capacity(64);
        for b in actual_bytes.iter() {
            actual_hex_str.push_str(&format!("{:02x}", b));
        }
        if actual_hex_str != expected {
            let expected_digest = hex_to_digest(expected).unwrap_or(Digest([0u8; 32]));
            let actual_digest =
                hex_to_digest(&actual_hex_str).unwrap_or(Digest([0xff_u8; 32]));
            return Err(LightrError::Integrity {
                // sha256 bytes stored in Digest (not blake3) — see module doc
                expected: expected_digest,
                actual: actual_digest,
            });
        }
    }

    Ok(())
}

/// Read the full body of a `ureq::Response` into a `Vec<u8>`.
pub(super) fn read_response_bytes(resp: ureq::Response) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(LightrError::Io)?;
    Ok(buf)
}

/// Choose the URL scheme for a registry host.
///
/// `localhost` / `127.0.0.1` (with or without a `:port`) → `http://` so a plain
/// local registry (the common `registry:2` dev setup) works without TLS; every
/// other host → `https://`. Pull is unaffected — only `push` calls this.
pub(super) fn registry_scheme(registry: &str) -> &'static str {
    let host = registry.split(':').next().unwrap_or(registry);
    if host == "localhost" || host == "127.0.0.1" {
        "http://"
    } else {
        "https://"
    }
}
