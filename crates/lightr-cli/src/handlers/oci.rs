//! `lightr oci` handlers — build-spec-r2 §4.
//!
//! Sub-verbs:
//!   oci import <layout-dir|tar> --name <ref> [--json]
//!   oci pull   <image>          --name <ref> [--json]

use lightr_core::validate_ref_name;
use lightr_oci::{import_layout, pull};
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
}
