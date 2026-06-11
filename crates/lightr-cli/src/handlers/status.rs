//! `lightr status` handler — build-spec v2 §7.
//!
//! Clean: prints `clean`, exits 0.
//! Dirty: prints blocks added:/removed:/changed: with prefixed paths, exits 1.
//!
//! JSON: {"clean":bool,"added":[],"removed":[],"changed":[]}, exit codes same.

use lightr_index::status;
use lightr_store::Store;
use serde::Serialize;

use crate::exit::{die_from_error, exit_dirty, exit_ok};

#[derive(Serialize)]
struct StatusJson {
    clean: bool,
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<String>,
}

pub fn run(dir: &str, name: &str, json: bool, _explain: bool) -> ! {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => die_from_error(&e),
    };

    let dir_path = std::path::Path::new(dir);
    let report = match status(dir_path, &store, name) {
        Ok(r) => r,
        Err(e) => die_from_error(&e),
    };

    let is_clean = report.clean;

    if json {
        let out = StatusJson {
            clean: report.clean,
            added: report.added,
            removed: report.removed,
            changed: report.changed,
        };
        println!("{}", serde_json::to_string(&out).expect("serialize status"));
    } else if is_clean {
        println!("clean");
    } else {
        if !report.added.is_empty() {
            println!("added:");
            for p in &report.added {
                println!("  + {p}");
            }
        }
        if !report.removed.is_empty() {
            println!("removed:");
            for p in &report.removed {
                println!("  - {p}");
            }
        }
        if !report.changed.is_empty() {
            println!("changed:");
            for p in &report.changed {
                println!("  ~ {p}");
            }
        }
    }

    if is_clean {
        exit_ok()
    } else {
        exit_dirty()
    }
}
