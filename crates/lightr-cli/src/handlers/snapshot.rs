//! `lightr snapshot` handler — build-spec v2 §7.
//!
//! Human output: `root=<hex16> files=<n> bytes=<n> new_objects=<n>`
//! JSON output:  {"root":"<full hex>","files":n,"bytes_total":n,"objects_new":n}
//!
//! --explain (≤4 lines to stderr):
//!   lightr: explain snapshot: files=<n> objects_new=<n>

use lightr_index::snapshot;
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_internal;

#[derive(Serialize)]
struct SnapshotJson {
    root: String,
    files: u64,
    bytes_total: u64,
    objects_new: u64,
}

pub fn run(dir: &str, name: &str, json: bool, explain: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_internal(&e),
    };

    let dir_path = std::path::Path::new(dir);
    let report = match snapshot(dir_path, &store, name) {
        Ok(r) => r,
        Err(e) => return die_internal(&e),
    };

    if explain {
        eprintln!(
            "lightr: explain snapshot: files={} objects_new={}",
            report.files, report.objects_new
        );
    }

    if json {
        let out = SnapshotJson {
            root: report.root.to_hex(),
            files: report.files,
            bytes_total: report.bytes_total,
            objects_new: report.objects_new,
        };
        println!(
            "{}",
            serde_json::to_string(&out).expect("serialize snapshot")
        );
    } else {
        let hex = report.root.to_hex();
        let short = &hex[..16];
        println!(
            "root={} files={} bytes={} new_objects={}",
            short, report.files, report.bytes_total, report.objects_new
        );
    }

    0
}
