//! `lightr history <ref>` handler — the top-level `docker history` verb mapped
//! onto the lightr ref registry (WP-IMAGE-VERBS).
//!
//! LEAD DESIGN (frozen): a Docker "image" = a named lightr ref. lightr's CAS
//! step-memo model does NOT persist a per-Dockerfile-instruction LAYER stack the
//! way Docker does, so this verb does NOT fabricate Docker's stacked-layer rows.
//! Instead it shows the ref's VERSION HISTORY — every `RefRecord` ever written
//! under the name ([`Store::ref_log`], newest-first), one row per version:
//!
//!   IMAGE ID        CREATED       COMMENT
//!   <short digest>  YYYY-MM-DD    <-- the manifest root of that ref version
//!
//! ## Honest layer-gap note (TRANSCRIPTION, not Docker-identical)
//!
//! Docker `history` lists the stacked filesystem layers + the instruction that
//! produced each (`docker history` SIZE / CREATED BY columns). lightr does not
//! retain that per-instruction layer breakdown in its step-memo CAS model
//! (chunks are content-addressed and shared, not stacked per instruction), so
//! THIS verb shows the ref's version log rather than a layer stack. A one-line
//! note is printed to STDERR (so `--json` / piped stdout stays clean) making
//! the difference explicit. The faithful per-layer breakdown — where an OCI
//! image config WAS retained (pulled/imported images) — lives under
//! `lightr oci history`, which reads the retained config's `history` array.
//!
//! Exit codes: absent ref ⇒ 1 (parity — a missing image is a runtime error,
//! Docker `history <missing>` exits 1). Store fault ⇒ 1.
//!
//! Memo: registry read only — touches no build/run memo keys.

use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

/// The honest note printed to stderr clarifying the layer-stack gap.
const LAYER_GAP_NOTE: &str =
    "note: lightr does not persist a per-instruction layer stack (CAS step-memo \
     model); showing the ref's version history. For a retained OCI image's \
     per-layer breakdown, use `lightr oci history`.";

#[derive(Serialize)]
struct HistoryJson {
    image_id: String,
    digest: String,
    created: String,
    created_at_unix: u64,
}

/// `lightr history <ref> [--json]`.
pub fn run(reference: &str, json: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    // Fail-closed on an absent ref: Docker `history <missing>` exits 1.
    match store.ref_get(reference) {
        Ok(Some(_)) => {}
        Ok(None) => {
            eprintln!("Error: No such image: {reference}");
            return 1;
        }
        Err(e) => return die_lightr(&e),
    }

    let log = match store.ref_log(reference) {
        Ok(l) => l,
        Err(e) => return die_lightr(&e),
    };

    // The honest layer-gap note goes to STDERR (keeps stdout/json clean).
    eprintln!("{LAYER_GAP_NOTE}");

    if json {
        print_json(&log);
    } else {
        print_table(&log);
    }
    0
}

/// Emit the version log as a JSON array (newest-first).
fn print_json(log: &[lightr_core::RefRecord]) {
    let out: Vec<HistoryJson> = log
        .iter()
        .map(|rec| {
            let hex = rec.root.to_hex();
            HistoryJson {
                image_id: short_hex(&hex),
                digest: hex,
                created: created_label(rec.created_at_unix),
                created_at_unix: rec.created_at_unix,
            }
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string(&out).expect("serialize history")
    );
}

/// Emit the version-log table: header always, one row per ref version
/// (newest-first). Empty log → just the header.
fn print_table(log: &[lightr_core::RefRecord]) {
    println!("IMAGE ID\tCREATED\tCOMMENT");
    for rec in log {
        let hex = rec.root.to_hex();
        println!(
            "{}\t{}\tref version",
            short_hex(&hex),
            created_label(rec.created_at_unix),
        );
    }
}

/// The 12-char short hex (Docker's IMAGE ID width); guarded for safety.
fn short_hex(full_hex: &str) -> String {
    let n = full_hex.len().min(12);
    full_hex[..n].to_string()
}

/// Render a unix timestamp as a UTC `YYYY-MM-DD` date, or `<unknown>` for a
/// zero timestamp (honest — never a fabricated date).
fn created_label(secs: u64) -> String {
    if secs == 0 {
        return "<unknown>".to_string();
    }
    let (y, m, d) = civil_from_unix_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days-since-epoch → civil (year, month, day), Howard Hinnant's algorithm
/// (public-domain). Used so CREATED needs no date crate.
fn civil_from_unix_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
