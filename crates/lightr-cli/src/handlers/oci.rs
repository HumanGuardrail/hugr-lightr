//! `lightr oci` handlers — build-spec-r2 §4 (dispatch entry).
//!
//! Sub-verbs:
//!   oci import  <layout-dir|tar> --name <ref> [--json]
//!   oci pull    <image>          --name <ref> [--json]
//!   oci push    <store-ref> <target-ref>      [--json]
//!   oci tag/save/load/images/rmi             (image-ops → `oci_imageops.rs`)
//!   oci history <ref>                         [--json]
//!
//! ## File split (WP-IMG-08, behavior-preserving)
//!
//! This file was AT the 400-LOC godfile cap, so the `docker tag/save/load/
//! images/rmi` parity verbs were moved verbatim to a sibling `oci_imageops.rs`
//! (declared below via `#[path]`, kept private to this module) and `pub use`d
//! back so callers still reach `handlers::oci::tag` / `…::images` / etc.
//! unchanged. This file keeps the registry verbs (import/pull/push), the new
//! `history` verb, and the shared report types.

use lightr_core::validate_ref_name;
use lightr_oci::{import_layout, pull, push};
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

// Image-ops verbs live in a private sibling file (split for the godfile cap);
// re-export so the dispatch + the docker shim keep calling `handlers::oci::*`.
#[path = "oci_imageops.rs"]
mod oci_imageops;
#[cfg(test)]
pub(crate) use oci_imageops::tag_in_store;
pub use oci_imageops::{images, load, rmi, save, tag};

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

// ── oci history ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct HistoryJson {
    created_by: String,
    /// Layer size in bytes; `null` when the size is unknown (`<missing>`).
    size: Option<u64>,
}

/// `oci history <ref>` — show the per-layer build history of an image
/// (docker `history`), WP-IMG-08. Reads the captured OCI config (IMG-01's
/// retained config + manifest sidecars): one row per build step with its
/// CREATED-BY instruction and the positional layer SIZE; `<missing>` is printed
/// honestly for layers/entries without provenance.
///
/// Surface note: the frozen `History` subcommand exposes ONLY `<ref>` and
/// `--json`. Docker's `--no-trunc` and `-q/--quiet` are NOT on the surface, so
/// they are DEFERRED here (no enum edit — see the WP card). The default text
/// output is already untruncated (full CREATED-BY), so `--no-trunc` is a no-op
/// today; `-q` (layer-id only) needs the layer digests on the surface.
///
/// Fail-closed: an absent/empty ref → exit 2 (RefNotFound); an image with no
/// retained provenance → exit 1 (InvalidManifest) — never a silent empty table.
pub fn history(target: &str, json: bool) -> i32 {
    // Validate the ref name first — exit 2 on invalid (matches the other verbs).
    if let Err(e) = validate_ref_name(target) {
        return die_lightr(&e);
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let rows = match lightr_oci::image_history(&store, target) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    if json {
        print_history_json(&rows);
    } else {
        print_history_table(&rows);
    }
    0
}

/// Emit the history rows as a JSON array (newest-first, docker order).
fn print_history_json(rows: &[lightr_oci::HistoryRow]) {
    let out: Vec<HistoryJson> = rows
        .iter()
        .map(|r| HistoryJson {
            created_by: r.created_by.clone(),
            size: r.size,
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string(&out).expect("serialize history list")
    );
}

/// Emit the docker-`history`-shaped table: header always, one row per layer
/// (newest-first). SIZE is human-readable (docker units); a layer with no
/// known size prints `<missing>` rather than a fabricated zero.
fn print_history_table(rows: &[lightr_oci::HistoryRow]) {
    println!("CREATED BY\tSIZE");
    for r in rows {
        let size = match r.size {
            Some(bytes) => oci_imageops::human_size(bytes),
            None => lightr_oci::MISSING.to_string(),
        };
        println!("{}\t{}", r.created_by, size);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "oci_tests.rs"]
mod tests;
