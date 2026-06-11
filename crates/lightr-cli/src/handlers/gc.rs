//! `lightr gc` handler — garbage collect unreachable objects.

use lightr_index::gc;
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_internal;

#[derive(Serialize)]
struct GcJson {
    objects_total: u64,
    reachable: u64,
    swept: u64,
    bytes_freed: u64,
    run_dirs_removed: u64,
}

pub fn run(force: bool, min_age: u64, json: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_internal(&e),
    };

    let dry_run = !force;
    let report = match gc(&store, dry_run, min_age) {
        Ok(r) => r,
        Err(e) => return die_internal(&e),
    };

    if json {
        let out = GcJson {
            objects_total: report.objects_total,
            reachable: report.reachable,
            swept: report.swept,
            bytes_freed: report.bytes_freed,
            run_dirs_removed: report.run_dirs_removed,
        };
        println!("{}", serde_json::to_string(&out).expect("serialize gc"));
    } else if dry_run {
        println!(
            "would sweep {} objects ({} bytes), {} run dirs — pass --force",
            report.swept, report.bytes_freed, report.run_dirs_removed
        );
    } else {
        println!(
            "swept {} objects ({} bytes), {} run dirs",
            report.swept, report.bytes_freed, report.run_dirs_removed
        );
    }

    0
}
