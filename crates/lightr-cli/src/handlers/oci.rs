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

// ── image ops ─ `docker tag/save/load/images/rmi/history` → `oci <verb>` ───────
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

/// `oci save <store-ref> [--output]` — export an image to a tar (docker save,
/// WP-IMG-04). Faithful (verbatim) when a retained image record exists, else a
/// synthesized single-layer fallback (reported honestly). Output goes to
/// `--output <file>` or stdout (Docker default); the status line goes to stderr
/// so a piped-to-stdout tar is never polluted. Fail-closed: absent ref → exit 2,
/// unwritable path → exit 1.
pub fn save(store_ref: &str, output: Option<&str>) -> i32 {
    if let Err(e) = validate_ref_name(store_ref) {
        return die_lightr(&e);
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let out_path = output.map(std::path::Path::new);
    let report = match lightr_oci::save(store_ref, out_path, &store) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    // Human-readable status to STDERR (stdout may carry the tar bytes).
    let fidelity = if report.faithful {
        "faithful"
    } else {
        "synthesized (lossy: no retained image record)"
    };
    eprintln!(
        "saved {} layers={} size={} fidelity={}",
        report.destination, report.layers, report.size, fidelity
    );
    0
}

/// `oci load [-i in.tar]` — import an image from a tar (docker load, WP-IMG-05).
///
/// The verb-level inverse of `oci save`: read a `docker save`/OCI-layout tar from
/// `-i <file>` or stdin (Docker's default), import it via the RETAINING path
/// (IMG-01 — original blobs + manifest retained, so the loaded image is
/// faithfully re-pushable and `load(save(x))` reproduces `x`), and tag it under
/// the ref embedded in the tar's `RepoTags` (or a deterministic content fallback
/// when the save carried no tag). The resolved ref + root go to STDERR (stdout
/// stays clean). Fail-closed: a malformed/absent tar → exit 1; an unsanitizable
/// tag → exit 2.
pub fn load(input: Option<&str>) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let in_path = input.map(std::path::Path::new);
    let report = match lightr_oci::load(in_path, &store) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    let hex = report.root.to_hex();
    let short = &hex[..16];
    let origin = if report.tagged_from_tar {
        "from RepoTags"
    } else {
        "untagged save (content fallback)"
    };
    eprintln!(
        "loaded name={} root={} layers={} files={} ({origin})",
        report.name, short, report.layers, report.files
    );
    0
}

/// `oci images` — list stored images (docker `docker images`), WP-IMG-06.
///
/// Columns: REPOSITORY, TAG, IMAGE ID, SIZE. Each stored ref is parsed into
/// `repo:tag` (a ref with no `:` → TAG `<none>`, as docker prints); IMAGE ID is
/// the 12-char short hex of the ref's root digest; SIZE is the total bytes of
/// the UNIQUE CAS objects reachable from that ref (each object counted once).
/// `--json` emits the rows as a JSON array. Fail-closed on any store error;
/// an empty store prints just the header (nothing for JSON: `[]`).
///
/// Surface note: the frozen `Images` subcommand exposes ONLY `--json`. Docker's
/// `-q/--quiet` and `--digests` are NOT on the surface, so they are deferred
/// here (the core `list_images` already returns the short id + full digest, so
/// adding them is a pure CLI-surface change when the enum is unfrozen).
pub fn images(json: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let rows = match lightr_oci::list_images(&store) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    if json {
        print_images_json(&rows);
    } else {
        print_images_table(&rows);
    }
    0
}

#[derive(Serialize)]
struct ImageJson {
    repository: String,
    tag: String,
    id: String,
    digest: String,
    size: u64,
}

/// Emit the rows as a JSON array (empty store → `[]`).
fn print_images_json(rows: &[lightr_oci::ImageRow]) {
    let out: Vec<ImageJson> = rows
        .iter()
        .map(|r| ImageJson {
            repository: r.repository.clone(),
            tag: r.tag.clone(),
            id: r.image_id.clone(),
            digest: r.digest.clone(),
            size: r.size,
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string(&out).expect("serialize images list")
    );
}

/// Emit the docker-`images`-shaped table: header always, one tab-aligned row
/// per image. Empty store → just the header.
fn print_images_table(rows: &[lightr_oci::ImageRow]) {
    println!("REPOSITORY\tTAG\tIMAGE ID\tSIZE");
    for r in rows {
        println!(
            "{}\t{}\t{}\t{}",
            r.repository,
            r.tag,
            r.image_id,
            human_size(r.size)
        );
    }
}

/// Render a byte count the way docker does (B/KB/MB/GB, base-1000, one decimal
/// above bytes). Keeps the SIZE column docker-faithful at a glance.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes}B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit < UNITS.len() - 1 {
        size /= 1000.0;
        unit += 1;
    }
    format!("{size:.1}{}", UNITS[unit])
}

/// `oci rmi <targets...>` — remove image ref(s), docker-faithful (WP-IMG-07).
/// Thin wiring (logic/render/exit-code live in `lightr_oci`; CAS objects left as
/// gc candidates). In-use = rootfs refs of running instances; `-f` bypasses.
pub fn rmi(targets: &[String], force: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };
    let in_use: Vec<String> = lightr_run::ps(&crate::lightr_home())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r.running.then_some(r.rootfs_ref).flatten())
        .collect();
    lightr_oci::render_rmi_results(&lightr_oci::rmi_many(&store, targets, &in_use, force))
}

/// `oci history <target>` — show the layer history of an image (docker history).
pub fn history(_target: &str, _json: bool) -> i32 {
    stub("oci history", "WP-IMG-03")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "oci_tests.rs"]
mod tests;
