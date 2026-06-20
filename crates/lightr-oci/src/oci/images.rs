//! WP-IMG-06 — `oci images`: list local images (docker `docker images`).
//!
//! Enumerates every stored ref ([`Store::list_refs`]) and renders one row per
//! ref in the docker-`images` shape: REPOSITORY, TAG, IMAGE ID, SIZE — plus the
//! full root DIGEST (the CLI surfaces it under `--digests`). The heavy logic
//! lives here (a thin handler in lightr-cli formats the table/quiet/json), so
//! `handlers/oci.rs` stays well under the godfile bound.
//!
//! ## repo:tag parsing (transcription note)
//!
//! Docker splits an image name into `repository:tag`. A lightr ref name CANNOT
//! contain `:` (ADR-0004 grammar: `^(@[a-z0-9-]{1,32}/)?[a-z0-9._-]{1,64}$`), so
//! a stored ref is a single token with no recoverable embedded tag — the docker
//! shim already sanitized any `repo:tag` into `repo-tag` at load/pull time. We
//! parse defensively with `rsplit_once(':')` (faithful to docker, and forward-
//! compatible if a future ref ever carries one); when no `:` is present the TAG
//! column is `<none>`, exactly as docker prints for an untagged image.
//!
//! ## SIZE semantics (unique objects, counted once)
//!
//! SIZE is the total bytes of the UNIQUE CAS objects reachable from the ref's
//! root: the root manifest object itself + each distinct `Entry::File` blob
//! (deduped by digest, so a tree that references the same blob twice counts it
//! once). Directory/symlink entries carry no CAS object. Fail-closed: an
//! unreadable root or undecodable manifest surfaces as an error (never a silent
//! zero), so the listing is honest.

use std::collections::HashSet;

use lightr_core::{Digest, Entry, Manifest, Result};
use lightr_store::Store;

/// Sentinel docker prints for the TAG column of an untagged image.
pub const NONE_TAG: &str = "<none>";

/// One rendered image row (docker `images` shape). `image_id` is the 12-char
/// short hex of the root digest; `digest` is the full 64-char root hex (surfaced
/// only under `--digests`); `size` is the unique-objects-once byte total.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRow {
    pub repository: String,
    pub tag: String,
    pub image_id: String,
    pub digest: String,
    pub size: u64,
}

/// List every stored image as an [`ImageRow`], sorted by ref name ascending
/// (`Store::list_refs` is already sorted). Fail-closed on any store error.
///
/// One row per ref. A ref whose root manifest is unreadable/undecodable fails
/// the whole listing (honest error over a partial/lying table).
pub fn list_images(store: &Store) -> Result<Vec<ImageRow>> {
    let names = store.list_refs()?;
    let mut rows = Vec::with_capacity(names.len());
    for name in names {
        // A name from list_refs may have no current ref record (e.g. a name
        // record written then the ref removed). Skip such danglers — there is
        // no image to list. (No removal path ships yet, so this is defensive.)
        let Some(rec) = store.ref_get(&name)? else {
            continue;
        };
        let (repository, tag) = parse_repo_tag(&name);
        let full_hex = rec.root.to_hex();
        let image_id = short_hex(&full_hex);
        let size = reachable_unique_size(store, &rec.root)?;
        rows.push(ImageRow {
            repository,
            tag,
            image_id,
            digest: full_hex,
            size,
        });
    }
    Ok(rows)
}

/// Split a ref name into `(repository, tag)`. No `:` ⇒ tag is [`NONE_TAG`]
/// (docker's untagged sentinel). See the module note on why `:` never appears
/// in a valid lightr ref name today.
fn parse_repo_tag(name: &str) -> (String, String) {
    match name.rsplit_once(':') {
        Some((repo, tag)) if !repo.is_empty() && !tag.is_empty() => {
            (repo.to_string(), tag.to_string())
        }
        _ => (name.to_string(), NONE_TAG.to_string()),
    }
}

/// The 12-char short hex docker prints for IMAGE ID (full hex is 64 chars, so
/// the slice is always in bounds for a real digest; guarded for safety).
fn short_hex(full_hex: &str) -> String {
    let n = full_hex.len().min(12);
    full_hex[..n].to_string()
}

/// Sum the bytes of the UNIQUE CAS objects reachable from `root`: the root
/// manifest object + each distinct `Entry::File` blob (deduped by digest).
/// Fail-closed: an unreadable root / undecodable manifest is an error.
fn reachable_unique_size(store: &Store, root: &Digest) -> Result<u64> {
    let manifest_bytes = store.get_bytes(root)?;
    let manifest = Manifest::decode(&manifest_bytes)?;

    let mut seen: HashSet<Digest> = HashSet::new();
    // The root manifest object itself counts once.
    seen.insert(*root);
    let mut total: u64 = manifest_bytes.len() as u64;

    for entry in &manifest.entries {
        if let Entry::File { digest, size, .. } = entry {
            // Dedup by digest — the same blob referenced twice counts once.
            if seen.insert(*digest) {
                total = total.saturating_add(*size);
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
#[path = "tests/images_tests.rs"]
mod images_tests;
