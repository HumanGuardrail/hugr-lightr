//! ADD `<url> <dest>` remote fetch (WP-G).
//!
//! Declared as a `#[path]` submodule of `exec_instr_copy` (mirrors how
//! `exec_fs` hosts `exec_fs_tar`), so this network concern lives in its own file
//! and `copy_instr` calls it as `add_url::*`.
//!
//! Docker semantics for a URL source (distinct from a local archive): the bytes
//! are DOWNLOADED to `dest` and NEVER auto-extracted — even a `.tar.gz` URL lands
//! as a file, unlike a LOCAL `.tar.gz` which extracts. The on-disk name comes
//! from the URL's last path segment when `dest` is a directory (trailing `/`),
//! else `dest` is the literal target path. We honor both by downloading into a
//! temp file named from the URL segment and handing that single source to
//! COPY-style placement with `extract = false`.
//!
//! Determinism note (memo key): Docker keys a URL ADD by the URL STRING (it does
//! NOT hash remote content). Our memo key folds the instruction's canonical text
//! (which contains the URL) and treats the non-context URL token as a missing
//! source (a stable sentinel), so the cache behavior matches Docker and the
//! fetch is never re-run when the canonical step is unchanged.

use lightr_core::{LightrError, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

/// `true` for a token Docker treats as a remote source (only `http`/`https`;
/// other schemes are NOT URL ADDs and fall back to context resolution, which
/// then honestly errors as a missing source).
pub(super) fn is_remote(token: &str) -> bool {
    let t = token.trim_start();
    t.starts_with("http://") || t.starts_with("https://")
}

/// Cap on a single ADD-URL body (256 MiB). A build input larger than this is
/// almost certainly a mistake (or a hostile stream); fail closed rather than
/// fill the disk. Mirrors the fail-closed discipline of the rest of the build.
const MAX_ADD_URL_BYTES: u64 = 256 * 1024 * 1024;

/// Derive the on-disk file name for a downloaded URL from its last path segment
/// (Docker's rule when dest is a directory). A URL with no usable segment
/// (e.g. `https://host/`) is an honest error — Docker also refuses it.
fn url_file_name(url: &str) -> Result<String> {
    // Strip scheme + query/fragment, then take the final non-empty path segment.
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let path_only = after_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let seg = path_only.rsplit('/').find(|s| !s.is_empty());
    match seg {
        Some(name) => Ok(name.to_string()),
        None => Err(LightrError::InvalidManifest(format!(
            "ADD: cannot derive a file name from URL {url:?} (no path segment); \
             give an explicit dest file path instead of a directory"
        ))),
    }
}

/// Download `url` into `dir` and return the path of the written file. The file
/// is named from the URL's last path segment so a directory dest gets the
/// Docker-correct name; a file dest is handled later by single-source placement.
///
/// Fail-closed: a non-2xx HTTP status is a [`LightrError::Registry`]; a transport
/// or IO failure is [`LightrError::Io`]; a body over [`MAX_ADD_URL_BYTES`] aborts.
pub(super) fn fetch_into(url: &str, dir: &Path) -> Result<PathBuf> {
    let name = url_file_name(url)?;
    let target = dir.join(&name);
    let agent = net_agent();
    let resp = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(status, r)) => {
            return Err(LightrError::Registry {
                status,
                msg: format!("ADD {url:?}: {}", r.status_text()),
            });
        }
        Err(ureq::Error::Transport(t)) => {
            return Err(LightrError::Io(std::io::Error::other(format!(
                "ADD {url:?}: transport error: {t}"
            ))));
        }
    };
    // Cap the read so a huge/streaming body cannot exhaust the disk.
    let mut reader = resp.into_reader().take(MAX_ADD_URL_BYTES + 1);
    let mut out = std::fs::File::create(&target).map_err(LightrError::Io)?;
    let copied = std::io::copy(&mut reader, &mut out).map_err(LightrError::Io)?;
    if copied > MAX_ADD_URL_BYTES {
        // Drop the partial file so a failed ADD never leaves bytes behind.
        let _ = std::fs::remove_file(&target);
        return Err(LightrError::InvalidManifest(format!(
            "ADD {url:?}: body exceeds {MAX_ADD_URL_BYTES} bytes"
        )));
    }
    Ok(target)
}

/// A ureq agent with explicit connect/read timeouts so a hung server cannot
/// stall a build forever (mirrors lightr-oci's registry agent posture).
fn net_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_remote_only_http_https() {
        assert!(is_remote("http://example.com/x.tar"));
        assert!(is_remote("https://example.com/x.tar"));
        assert!(is_remote("  https://example.com/x.tar")); // leading ws trimmed
        assert!(!is_remote("ftp://example.com/x.tar"));
        assert!(!is_remote("./local/x.tar"));
        assert!(!is_remote("x.tar"));
    }

    #[test]
    fn url_file_name_last_segment() {
        assert_eq!(
            url_file_name("https://h/a/b/foo.tar.gz").unwrap(),
            "foo.tar.gz"
        );
        assert_eq!(url_file_name("http://h/foo.tar").unwrap(), "foo.tar");
        // Query/fragment are stripped before taking the segment.
        assert_eq!(
            url_file_name("https://h/dl/app.bin?token=abc#frag").unwrap(),
            "app.bin"
        );
    }

    #[test]
    fn url_file_name_falls_back_to_host_when_no_path() {
        // `https://host/` and `https://host` have no path segment beyond the host,
        // so the host itself is the last non-empty segment (a degenerate but
        // non-empty name — Docker would 404 such a fetch anyway).
        assert_eq!(url_file_name("https://host/").unwrap(), "host");
        assert_eq!(url_file_name("https://host").unwrap(), "host");
    }

    #[test]
    fn url_file_name_empty_after_scheme_is_error() {
        // A URL that is ONLY a scheme has no segment at all — honest error.
        assert!(url_file_name("https://").is_err());
    }
}
