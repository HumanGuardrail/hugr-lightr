//! `lightr oci` handlers — build-spec-r2 §4.
//!
//! Sub-verbs:
//!   oci import <layout-dir|tar> --name <ref> [--json]
//!   oci pull   <image>          --name <ref> [--json]
//!   oci push   <store-ref> <target-ref>      [--json]

use lightr_core::validate_ref_name;
use lightr_oci::{import_layout, pull, push};
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

// ── Shared output type ────────────────────────────────────────────────────────

#[derive(Serialize)]
struct OciJson {
    name: String,
    root: String,
    layers: u64,
    files: u64,
}

fn print_report(report: &lightr_oci::ImportReport, json: bool) {
    if json {
        let out = OciJson {
            name: report.name.clone(),
            root: report.root.to_hex(),
            layers: report.layers,
            files: report.files,
        };
        println!(
            "{}",
            serde_json::to_string(&out).expect("serialize oci report")
        );
    } else {
        let hex = report.root.to_hex();
        let short = &hex[..16];
        println!(
            "name={} root={} layers={} files={}",
            report.name, short, report.layers, report.files
        );
    }
}

// ── oci import ────────────────────────────────────────────────────────────────

pub fn import(path: &str, name: &str, json: bool) -> i32 {
    // Validate ref name — exit 2 on invalid
    if let Err(e) = validate_ref_name(name) {
        return die_lightr(&e);
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let layout_path = std::path::Path::new(path);
    let report = match import_layout(layout_path, &store, name) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    print_report(&report, json);
    0
}

// ── oci pull ──────────────────────────────────────────────────────────────────

pub fn pull_image(image: &str, name: &str, json: bool) -> i32 {
    // Validate ref name — exit 2 on invalid
    if let Err(e) = validate_ref_name(name) {
        return die_lightr(&e);
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let report = match pull(image, &store, name) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
        // Note: die_lightr maps RefNotFound/InvalidRef → 2, everything else → 1.
        // Network/registry errors (Io) map to exit 1 per §4.
    };

    print_report(&report, json);
    0
}

// ── oci push ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct OciPushJson {
    target: String,
    manifest_digest: String,
    layers: u64,
    size: u64,
}

fn print_push_report(report: &lightr_oci::PushReport, json: bool) {
    if json {
        let out = OciPushJson {
            target: report.target.clone(),
            manifest_digest: report.manifest_digest.clone(),
            layers: report.layers,
            size: report.size,
        };
        println!(
            "{}",
            serde_json::to_string(&out).expect("serialize oci push report")
        );
    } else {
        println!(
            "target={} manifest={} layers={} size={}",
            report.target, report.manifest_digest, report.layers, report.size
        );
    }
}

/// `oci push <store-ref> <target-ref>` — synthesize a single-layer OCI image
/// from the stored tree and upload it to a registry. Mirrors `pull_image`.
pub fn push_image(store_ref: &str, target: &str, json: bool) -> i32 {
    // Validate the local ref name — exit 2 on invalid.
    if let Err(e) = validate_ref_name(store_ref) {
        return die_lightr(&e);
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let report = match push(store_ref, target, &store) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
        // die_lightr maps RefNotFound/InvalidRef → 2; registry/Io errors → 1.
    };

    print_push_report(&report, json);
    0
}

// ── image ops (CLI-surface freeze — honest fail-closed stubs) ──────────────────
//
// `docker tag/save/load/images/rmi/history` map to `oci <verb>` (the shim does
// the translation, untouched here). Behavior lands in WP-IMG-*.

use crate::handlers::stub::stub;

/// `oci tag <src> <target>` — alias an existing store ref to a new name,
/// Docker-faithful, with ZERO data copy (WP-IMG-03).
///
/// Resolves `src` → its root digest, then creates/repoints `target` at the same
/// root (last-write-wins on `target`). Fail-closed: an absent `src` is an error
/// (exit 2, RefNotFound) — never a silent empty alias. The image sidecars
/// (config + manifest, IMG-01) are copied to `target` so a later faithful
/// `oci push` of the alias reproduces the original image; no CAS object is
/// duplicated (only the ref pointer + the 32-byte sidecar pointers).
pub fn tag(src: &str, target: &str) -> i32 {
    // Validate both names first — exit 2 on invalid (matches push_image).
    if let Err(e) = validate_ref_name(src) {
        return die_lightr(&e);
    }
    if let Err(e) = validate_ref_name(target) {
        return die_lightr(&e);
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    match tag_in_store(&store, src, target) {
        Ok(()) => 0,
        Err(e) => die_lightr(&e),
    }
}

/// Core of `oci tag`, store injected (parallel-safe — no process-global env).
/// Returns `RefNotFound(src)` if `src` is absent (fail-closed). On success the
/// alias is written and the image sidecars are copied.
fn tag_in_store(store: &Store, src: &str, target: &str) -> lightr_core::Result<()> {
    use lightr_core::{LightrError, RefRecord};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Resolve src → its record. Absent ⇒ fail-closed (no silent empty alias).
    let src_rec = store
        .ref_get(src)?
        .ok_or_else(|| LightrError::RefNotFound(src.to_string()))?;

    // Repoint target at the SAME root (zero data copy). The alias derives from
    // src, so record src's root as the parent and preserve the original
    // tool_version (the image was built by that version — faithful fidelity).
    let created_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dst_rec = RefRecord {
        name: target.to_string(),
        root: src_rec.root,
        parent: Some(src_rec.root),
        created_at_unix,
        tool_version: src_rec.tool_version.clone(),
    };
    store.ref_put(&dst_rec)?;

    // Copy the image sidecars so the alias stays faithfully pushable. No-op if
    // src has no sidecar (tag still works). Shares CAS blobs — no duplication.
    store.copy_image_sidecars(src, target)?;
    Ok(())
}

/// `oci save <store-ref> [--output]` — export an image to a tar (docker save).
pub fn save(_store_ref: &str, _output: Option<&str>) -> i32 {
    stub("oci save", "WP-IMG-03")
}

/// `oci load [--input]` — import an image from a tar (docker load).
pub fn load(_input: Option<&str>) -> i32 {
    stub("oci load", "WP-IMG-03")
}

/// `oci images` — list stored images (docker images).
pub fn images(_json: bool) -> i32 {
    stub("oci images", "WP-IMG-03")
}

/// `oci rmi <targets...>` — remove one or more images (docker rmi).
pub fn rmi(_targets: &[String], _force: bool) -> i32 {
    stub("oci rmi", "WP-IMG-03")
}

/// `oci history <target>` — show the layer history of an image (docker history).
pub fn history(_target: &str, _json: bool) -> i32 {
    stub("oci history", "WP-IMG-03")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "oci_tests.rs"]
mod tests;
