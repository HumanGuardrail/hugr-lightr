//! WP-IMG-05 — `oci load [-i in.tar]`: import an image FROM a tar.
//!
//! The **verb-level inverse of `save`** (save.rs) and a thin shim over the
//! retaining import (import.rs): read a `docker save` / OCI-layout tar (from a
//! path or stdin), discover the ref name the image was saved under (the tar's
//! `RepoTags`), and delegate to [`import_layout`] so the ORIGINAL blobs +
//! manifest are RETAINED (WP-IMG-01) — i.e. a loaded image is faithfully
//! re-pushable, and `load(save(x))` reproduces `x`'s content + blob digests.
//!
//! Naming is Docker-faithful: `docker load` re-tags the image from its
//! `RepoTags`. We sanitize the first repo:tag into a valid lightr ref
//! (`@loaded/<repo>-<tag>`, the same `/`,`:`→`-` convention the docker shim
//! uses). A tag-less save (`docker save <image-id>`, no `RepoTags`) loads under
//! a deterministic content-addressed fallback `@loaded/img-<first12>` — never a
//! silent failure (mirrors Docker loading an untagged image by id).
//!
//! Fail-closed: a malformed/absent tar surfaces as an honest error from the
//! import path (`Io` → exit 1, `InvalidManifest` → exit 1); a tar whose only
//! candidate name is unsanitizable is `InvalidRef` (exit 2).

use super::import::import_layout;
use super::model::{DockerSaveItem, LoadReport};
use super::util::{sha256_hex_of, TempDirGuard};
use flate2::read::GzDecoder;
use lightr_core::{validate_ref_name, LightrError, Result};
use lightr_store::Store;
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

/// Namespace under which loaded images are tagged when the name is derived from
/// the tar (or synthesized). Keeps loaded refs grouped + grammar-valid.
const LOAD_NS: &str = "@loaded/";

/// Import an image from a `docker save` / OCI-layout tar.
///
/// `input` is a path (the `-i <file>` arg) or `None` to read the tar from
/// `stdin` (the Docker default). The tar is materialized to a private tempfile
/// and imported via the retaining [`import_layout`] path. The loaded image is
/// tagged under the ref embedded in the tar's `RepoTags` (sanitized), or a
/// deterministic content fallback when the save carried no tag.
///
/// Fail-closed: an unreadable input is `Io` (exit 1); a tar without a parseable
/// `manifest.json` is `InvalidManifest` (exit 1) via `import_layout`.
pub fn load(input: Option<&Path>, store: &Store) -> Result<LoadReport> {
    let raw = read_input(input)?;

    // Discover the ref name BEFORE import (RepoTags from manifest.json).
    let (name, tagged_from_tar) = resolve_name(&raw)?;

    // Materialize to a private tempfile so we can reuse the path-based retaining
    // import (which handles gzip + OCI-layout/docker-save routing itself).
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp_dir = std::env::temp_dir().join(format!("lightr-oci-load-{pid}-{nanos}"));
    fs::create_dir_all(&tmp_dir).map_err(LightrError::Io)?;
    let _guard = TempDirGuard(tmp_dir.clone());
    let tar_path = tmp_dir.join("in.tar");
    fs::write(&tar_path, &raw).map_err(LightrError::Io)?;

    let report = import_layout(&tar_path, store, &name)?;

    Ok(LoadReport {
        name,
        root: report.root,
        layers: report.layers,
        files: report.files,
        tagged_from_tar,
    })
}

/// Read the whole tar into memory: from `input` (a path) or stdin (`None`).
/// `docker save`/`load` tars are small enough to buffer; this keeps the
/// stdin/file paths uniform and the import reuse trivial.
fn read_input(input: Option<&Path>) -> Result<Vec<u8>> {
    match input {
        Some(path) => fs::read(path).map_err(LightrError::Io),
        None => {
            let mut buf = Vec::new();
            io::stdin()
                .lock()
                .read_to_end(&mut buf)
                .map_err(LightrError::Io)?;
            Ok(buf)
        }
    }
}

/// Resolve the local ref name for the loaded image.
///
/// Returns `(name, tagged_from_tar)`: the sanitized first `RepoTags` entry when
/// the tar names itself, else a deterministic content fallback derived from the
/// tar bytes' sha256. `tagged_from_tar` is `true` only in the former case.
fn resolve_name(raw: &[u8]) -> Result<(String, bool)> {
    if let Some(tag) = first_repo_tag(raw)? {
        let name = sanitize_ref(&tag);
        // The sanitized name must satisfy the lightr ref grammar; a degenerate
        // tag (e.g. only invalid chars) fails closed as a usage error.
        validate_ref_name(&name).map_err(|_| LightrError::InvalidRef(tag))?;
        Ok((name, true))
    } else {
        // Tag-less save: deterministic content-addressed fallback (load by id).
        let hex = sha256_hex_of(raw);
        let name = format!("{LOAD_NS}img-{}", &hex[..12]);
        Ok((name, false))
    }
}

/// Sanitize a docker `repo:tag` into a valid lightr ref name under `@loaded/`.
/// Mirrors the docker shim's `/`,`:` → `-` convention; lowercases and drops any
/// remaining out-of-grammar bytes, then bounds the local part to 64 chars
/// (ADR-0004 grammar). An empty result yields a stable `untagged` local part.
fn sanitize_ref(repo_tag: &str) -> String {
    let mut local: String = repo_tag
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == '/' || c == ':' { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-')
        .collect();
    if local.is_empty() {
        local = "untagged".to_string();
    }
    local.truncate(64);
    format!("{LOAD_NS}{local}")
}

/// Scan the (optionally gzipped) outer tar for `manifest.json` and return the
/// first `RepoTags` entry, if any. `None` when the tar has no `manifest.json`
/// (a bare OCI-layout dir-in-tar) or an empty/absent `RepoTags` — both legal;
/// the caller then uses the content fallback. A *broken* tar is reported as
/// `Io` (fail-closed); a present-but-unparseable `manifest.json` as
/// `InvalidManifest`.
fn first_repo_tag(raw: &[u8]) -> Result<Option<String>> {
    let tar_bytes: Vec<u8> = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut gz = GzDecoder::new(raw);
        let mut out = Vec::new();
        gz.read_to_end(&mut out).map_err(LightrError::Io)?;
        out
    } else {
        raw.to_vec()
    };

    let cursor = io::Cursor::new(&tar_bytes);
    let mut archive = tar::Archive::new(cursor);
    for entry_result in archive.entries().map_err(LightrError::Io)? {
        let mut entry = entry_result.map_err(LightrError::Io)?;
        let path = entry.path().map_err(LightrError::Io)?.into_owned();
        let path_str = path.to_string_lossy();
        if path_str == "manifest.json" || path_str == "./manifest.json" {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).map_err(LightrError::Io)?;
            let items: Vec<DockerSaveItem> = serde_json::from_slice(&buf).map_err(|e| {
                LightrError::InvalidManifest(format!("load: manifest.json parse error: {e}"))
            })?;
            let tag = items
                .into_iter()
                .next()
                .and_then(|it| it.repo_tags.into_iter().find(|t| !t.trim().is_empty()));
            return Ok(tag);
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "tests/load_tests.rs"]
mod load_tests;
