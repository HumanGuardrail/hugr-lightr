//! `lightr images` handler — the top-level `docker images` verb mapped onto the
//! lightr ref registry (WP-IMAGE-VERBS).
//!
//! LEAD DESIGN (frozen): a Docker "image" = a named lightr ref
//! (`RefRecord`: name → manifest digest). This verb TRANSCRIBES `docker images`
//! output onto [`Store::list_refs`]: one row per stored ref, in the Docker
//! column shape `REPOSITORY  TAG  IMAGE ID  CREATED  SIZE`.
//!
//! ## Column derivation (transcription notes — minimal Docker-faithful rules)
//!
//! - **REPOSITORY[:TAG]** — a lightr ref name cannot embed a `:` (ADR-0004
//!   grammar `^(@[a-z0-9-]{1,32}/)?[a-z0-9._-]{1,64}$`), so a stored ref is a
//!   single token. We split defensively on a trailing `:tag` (`rsplit_once(':')`,
//!   forward-compatible if a future ref ever carries one); with no `:` the TAG
//!   column is `<none>` — exactly what Docker prints for an untagged image.
//!   (We do NOT fabricate `latest`: an untagged image is `<none>` in Docker.)
//! - **IMAGE ID** — the 12-char short hex of the ref's root (manifest) digest.
//! - **CREATED** — derived from `RefRecord.created_at_unix` as a UTC date
//!   `YYYY-MM-DD`. The field is always present in lightr (set at snapshot/tag
//!   time); a record with a zero timestamp prints `<unknown>` (honest, never a
//!   fabricated date).
//! - **SIZE** — the summed bytes of the UNIQUE CAS objects reachable from the
//!   ref's root (root manifest object + each distinct file blob, deduped),
//!   human-readable (`4.2MB`). Reuses [`lightr_oci::list_images`], the proven
//!   sizing core — composed here, not reimplemented.
//!
//! `--json` emits the rows as a JSON array (matches the house `--json`).
//! `-q`/`--quiet` prints only the short IMAGE IDs, one per line (Docker parity).
//!
//! Memo: this verb is a registry read only — it touches no build/run memo keys.

use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

/// Sentinel Docker prints when the CREATED timestamp is unknown.
const UNKNOWN_CREATED: &str = "<unknown>";

/// Sentinel Docker prints for the TAG column of an untagged image. Mirrors
/// `lightr_oci::images::NONE_TAG` (which is `pub(crate)` in that crate, so it
/// cannot be imported here); kept in sync by the images_tests assertion.
const NONE_TAG: &str = "<none>";

#[derive(Serialize)]
struct ImageJson {
    repository: String,
    tag: String,
    id: String,
    digest: String,
    created: String,
    created_at_unix: u64,
    size: u64,
}

/// `lightr images [--quiet] [--json]`.
pub fn run(quiet: bool, json: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    // Reuse the proven sizing/repo:tag/id core (lightr_oci::list_images), then
    // enrich each row with CREATED from the ref record.
    let rows = match lightr_oci::list_images(&store) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    // Build the (row, created_at_unix) pairs. A row whose ref vanished between
    // list and lookup is skipped (defensive; list_images already filters).
    let mut enriched: Vec<(lightr_oci::ImageRow, u64)> = Vec::with_capacity(rows.len());
    for r in rows {
        let name = if r.tag == NONE_TAG {
            r.repository.clone()
        } else {
            format!("{}:{}", r.repository, r.tag)
        };
        let created = match store.ref_get(&name) {
            Ok(Some(rec)) => rec.created_at_unix,
            Ok(None) => 0,
            Err(e) => return die_lightr(&e),
        };
        enriched.push((r, created));
    }

    if quiet {
        for (r, _) in &enriched {
            println!("{}", r.image_id);
        }
    } else if json {
        print_json(&enriched);
    } else {
        print_table(&enriched);
    }
    0
}

/// Emit the rows as a JSON array (empty store → `[]`).
fn print_json(rows: &[(lightr_oci::ImageRow, u64)]) {
    let out: Vec<ImageJson> = rows
        .iter()
        .map(|(r, created)| ImageJson {
            repository: r.repository.clone(),
            tag: r.tag.clone(),
            id: r.image_id.clone(),
            digest: r.digest.clone(),
            created: created_label(*created),
            created_at_unix: *created,
            size: r.size,
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string(&out).expect("serialize images list")
    );
}

/// Emit the Docker-`images`-shaped table: header always, one tab-aligned row per
/// image. Empty store → just the header.
fn print_table(rows: &[(lightr_oci::ImageRow, u64)]) {
    println!("REPOSITORY\tTAG\tIMAGE ID\tCREATED\tSIZE");
    for (r, created) in rows {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            r.repository,
            r.tag,
            r.image_id,
            created_label(*created),
            human_size(r.size)
        );
    }
}

/// Render a unix timestamp as a UTC `YYYY-MM-DD` date, or [`UNKNOWN_CREATED`]
/// for a zero/unset timestamp (honest — never a fabricated date). Pure integer
/// civil-date math (no chrono dependency), valid for any post-epoch second.
fn created_label(secs: u64) -> String {
    if secs == 0 {
        return UNKNOWN_CREATED.to_string();
    }
    let (y, m, d) = civil_from_unix_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Render a byte count the way Docker does (B/KB/MB/GB, base-1000, one decimal
/// above bytes). Keeps the SIZE column Docker-faithful at a glance. (A local
/// copy of `lightr_oci`'s `human_size`, which lives in a `pub(crate)` module of
/// another crate and cannot be imported here.)
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

/// Convert days-since-epoch to a civil (year, month, day) via Howard Hinnant's
/// `civil_from_days` algorithm (public-domain, exact for the proleptic
/// Gregorian calendar). Used so CREATED needs no date crate.
fn civil_from_unix_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
