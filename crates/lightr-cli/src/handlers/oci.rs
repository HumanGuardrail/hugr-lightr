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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // Handler-level unit checks — no network; clap parse tests are in main.rs.

    #[test]
    fn import_bad_ref_exits_2() {
        // Uppercase name is invalid ref
        let code = super::import("/some/path", "INVALID", false);
        assert_eq!(code, 2, "bad ref name must exit 2");
    }

    #[test]
    fn import_empty_ref_exits_2() {
        let code = super::import("/some/path", "", false);
        assert_eq!(code, 2, "empty ref name must exit 2");
    }

    #[test]
    fn pull_bad_ref_exits_2() {
        let code = super::pull_image("alpine", "INVALID", false);
        assert_eq!(code, 2, "bad ref name must exit 2");
    }

    #[test]
    fn pull_empty_ref_exits_2() {
        let code = super::pull_image("alpine", "", false);
        assert_eq!(code, 2, "empty ref name must exit 2");
    }

    #[test]
    fn push_bad_ref_exits_2() {
        // Uppercase store-ref is an invalid ref name → exit 2.
        let code = super::push_image("INVALID", "localhost:5000/x:latest", false);
        assert_eq!(code, 2, "bad store-ref name must exit 2");
    }

    #[test]
    fn push_empty_ref_exits_2() {
        let code = super::push_image("", "localhost:5000/x:latest", false);
        assert_eq!(code, 2, "empty store-ref name must exit 2");
    }

    #[test]
    fn push_unknown_ref_exits_2() {
        // Valid name but absent ref → RefNotFound → exit 2 (no network touched).
        // Uses an isolated LIGHTR_HOME so it never hits the user's real store.
        let _guard = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", tmp.path());
        let code = super::push_image("@t/never-pushed", "localhost:5000/x:latest", false);
        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 2, "unknown ref must exit 2 (RefNotFound)");
    }
}
