//! `lightr tag <src> <dst>` handler — the top-level `docker tag` verb mapped
//! onto the lightr ref registry (WP-IMAGE-VERBS).
//!
//! LEAD DESIGN (frozen): a Docker "image" = a named lightr ref. `tag` ALIASES an
//! existing image's manifest under a new name: `ref_get(src)` → `ref_put` a new
//! `RefRecord{ name: dst, root: src.root, … }`. ZERO data copy — both refs point
//! at the same manifest digest and share every CAS chunk.
//!
//! Per Docker `docker tag`:
//!   • a missing `src` is an error `No such image: <src>` and exits **1**
//!     (Docker treats a missing source image as a runtime error, exit 1 — NOT a
//!     usage error);
//!   • on success Docker prints nothing (exit 0).
//!
//! The image sidecars (config + manifest record) are copied to `dst` so a later
//! faithful `oci push` of the alias reproduces the original image — sharing the
//! CAS blobs, no object duplication.
//!
//! Exit codes: missing src ⇒ 1 (parity). Invalid dst name (bad grammar) ⇒ 2
//! (usage). Store fault ⇒ 1.
//!
//! Memo: registry op only — touches no build/run memo keys.

use lightr_core::{validate_ref_name, LightrError, RefRecord};
use lightr_store::Store;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::exit::die_lightr;

/// `lightr tag <src> <dst>`.
pub fn run(src: &str, dst: &str) -> i32 {
    // An invalid dst NAME is a usage/arg-class error (exit 2). We validate dst
    // up-front; src is validated by the resolve path (a malformed src that names
    // no image surfaces "No such image", exit 1 — Docker faithful).
    if let Err(e) = validate_ref_name(dst) {
        return die_lightr(&e); // InvalidRef ⇒ exit 2
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    match tag_in_store(&store, src, dst) {
        Ok(()) => 0,
        Err(LightrError::RefNotFound(_)) => {
            // Docker shape: missing source image ⇒ "No such image" + exit 1.
            eprintln!("Error: No such image: {src}");
            1
        }
        Err(e) => die_lightr(&e),
    }
}

/// Core of `tag`, store injected (parallel-safe — no process-global env).
/// Returns `RefNotFound(src)` if `src` is absent (fail-closed; no silent empty
/// alias). On success the alias ref is written and the image sidecars copied.
pub(crate) fn tag_in_store(store: &Store, src: &str, dst: &str) -> lightr_core::Result<()> {
    let src_rec = store
        .ref_get(src)?
        .ok_or_else(|| LightrError::RefNotFound(src.to_string()))?;

    let created_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dst_rec = RefRecord {
        name: dst.to_string(),
        root: src_rec.root,
        // The alias derives from src — record src's root as the parent and keep
        // the original tool_version (the image was built by that version).
        parent: Some(src_rec.root),
        created_at_unix,
        tool_version: src_rec.tool_version.clone(),
    };
    store.ref_put(&dst_rec)?;

    // Share the image sidecars so the alias stays faithfully pushable. No-op if
    // src has no sidecar (tag still works). Shares CAS blobs — no duplication.
    store.copy_image_sidecars(src, dst)?;
    Ok(())
}

#[cfg(test)]
#[path = "tag_tests.rs"]
mod tests;
