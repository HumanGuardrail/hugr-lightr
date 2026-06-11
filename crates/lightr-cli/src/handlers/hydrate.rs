//! `lightr hydrate` handler — build-spec v2 §7.
//!
//! Human output: `root=<hex16> files=<n> bytes=<n> rung=<Clone|Reflink|CopyRange|Copy>`
//! JSON output:  {"root":"<full hex>","files":n,"bytes_total":n,"rung":"<lowercase>"}
//!
//! --explain (≤4 lines to stderr):
//!   lightr: explain hydrate: rung=<rung> files=<n>

use lightr_index::{hydrate, hydrate_verified};
use lightr_store::{CowRung, Store};
use serde::Serialize;

use crate::exit::{die_from_error, exit_ok};

#[derive(Serialize)]
struct HydrateJson {
    root: String,
    files: u64,
    bytes_total: u64,
    rung: String,
}

fn rung_str(r: CowRung) -> &'static str {
    match r {
        CowRung::Clone => "Clone",
        CowRung::Reflink => "Reflink",
        CowRung::CopyRange => "CopyRange",
        CowRung::Copy => "Copy",
    }
}

fn rung_lower(r: CowRung) -> &'static str {
    match r {
        CowRung::Clone => "clone",
        CowRung::Reflink => "reflink",
        CowRung::CopyRange => "copyrange",
        CowRung::Copy => "copy",
    }
}

pub fn run(dest: &str, name: &str, verify: bool, json: bool, explain: bool) -> ! {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => die_from_error(&e),
    };

    let dest_path = std::path::Path::new(dest);
    let result = if verify {
        hydrate_verified(dest_path, &store, name)
    } else {
        hydrate(dest_path, &store, name)
    };
    let report = match result {
        Ok(r) => r,
        Err(e) => die_from_error(&e),
    };

    if explain {
        eprintln!(
            "lightr: explain hydrate: rung={} files={}",
            rung_str(report.rung),
            report.files
        );
    }

    if json {
        let out = HydrateJson {
            root: report.root.to_hex(),
            files: report.files,
            bytes_total: report.bytes_total,
            rung: rung_lower(report.rung).to_string(),
        };
        println!(
            "{}",
            serde_json::to_string(&out).expect("serialize hydrate")
        );
    } else {
        let hex = report.root.to_hex();
        let short = &hex[..16];
        println!(
            "root={} files={} bytes={} rung={}",
            short,
            report.files,
            report.bytes_total,
            rung_str(report.rung)
        );
    }

    exit_ok()
}
