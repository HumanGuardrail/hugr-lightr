//! `lightr system` handlers — docker `system df` / `system prune` parity onto
//! the daemonless CAS model (WP-EDGE-VERBS).
//!
//! - `system df`: disk-usage report — Images (refs + CAS bytes), Build Cache
//!   (AC entries + bytes), and a reclaimable estimate.
//! - `system prune`: reclaim unused data by REUSING `gc` (never reimplemented);
//!   prints Docker's "Total reclaimed space: X". Like Docker prune (and gc),
//!   it NEVER removes tagged refs — only unreachable objects + stale run dirs.

use lightr_index::gc;
use lightr_store::Store;
use serde::Serialize;

use crate::cli::cmd::SystemCmd;
use crate::exit::die_lightr;

/// Render a byte count the way Docker does (B/KB/MB/GB, base-1000, one decimal
/// above bytes), e.g. `4.2MB`. Shared by `system df`, `system prune`, and
/// `info`. (A local copy of the same formatter used in the image verbs, which
/// lives in a private module of another crate.)
pub(crate) fn human_size(bytes: u64) -> String {
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

// ── system df ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(crate) struct DfRowJson {
    /// Row type, e.g. "Images" / "Build Cache".
    kind: &'static str,
    /// Item count for the row (refs / AC entries).
    total: u64,
    /// On-disk bytes attributed to the row.
    size: u64,
    /// COUNT of objects reclaimable by `system prune` — measured by a gc
    /// dry-run mark-walk. Tense-discipline: gc reports the unreachable-object
    /// count on a dry-run but NOT their byte total (bytes are only summed during
    /// an actual sweep), so we surface the honest measured count here and never
    /// fabricate a reclaimable-byte figure. `system prune --force` reports the
    /// real reclaimed bytes once the sweep runs.
    reclaimable_objects: u64,
}

#[derive(Serialize)]
pub(crate) struct DfJson {
    rows: Vec<DfRowJson>,
}

/// Collect the `system df` report from an already-open `store`. Read-only.
/// Factored out for parallel-safe tests (no process-global env).
pub(crate) fn gather_df(store: &Store) -> lightr_core::Result<DfJson> {
    let usage = store.store_usage()?;
    let refs = store.list_refs()?.len() as u64;

    // Build cache = the Action Cache. Count entries + sum their value bytes.
    let ac = store.list_ac()?;
    let ac_entries = ac.len() as u64;
    let ac_bytes: u64 = ac.iter().map(|v| v.len() as u64).sum();

    // Reclaimable OBJECT COUNT = what a gc dry-run (min_age 0) marks sweepable
    // now. Reuses the real gc mark-walk — never a separate estimate. (gc's
    // dry-run does not sum bytes, so we report the measured count, not bytes.)
    let reclaimable_objects = gc(store, true, 0).map(|r| r.swept).unwrap_or(0);

    Ok(DfJson {
        rows: vec![
            DfRowJson {
                kind: "Images",
                total: refs,
                size: usage.bytes,
                reclaimable_objects,
            },
            DfRowJson {
                kind: "Build Cache",
                total: ac_entries,
                size: ac_bytes,
                reclaimable_objects: 0,
            },
        ],
    })
}

fn df(json: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };
    let report = match gather_df(&store) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    if json {
        println!(
            "{}",
            serde_json::to_string(&report).expect("serialize system df")
        );
    } else {
        println!("{:<14}{:<8}{:<12}RECLAIMABLE", "TYPE", "TOTAL", "SIZE");
        for row in &report.rows {
            println!(
                "{:<14}{:<8}{:<12}{} objects",
                row.kind,
                row.total,
                human_size(row.size),
                row.reclaimable_objects,
            );
        }
    }
    0
}

// ── system prune ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct PruneJson {
    /// Whether objects were actually swept (`--force`) or only previewed.
    pruned: bool,
    /// Objects swept (force) or marked sweepable (dry-run preview).
    objects_swept: u64,
    /// Stale run dirs removed (0 on a dry-run preview).
    run_dirs_removed: u64,
    /// Bytes reclaimed by an actual sweep. gc only sums bytes during a real
    /// sweep, so on a dry-run preview this is 0 (the object COUNT above is the
    /// measured preview figure — tense-discipline: no fabricated byte estimate).
    reclaimed_bytes: u64,
}

fn prune(force: bool, min_age: u64, json: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    // REUSE gc: dry_run = !force. Docker prune removes dangling data, not
    // tagged images — gc's mark phase keeps every ref-reachable object alive,
    // so refs are never untagged here (matches gc's safety).
    let dry_run = !force;
    let report = match gc(&store, dry_run, min_age) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    if json {
        let out = PruneJson {
            pruned: force,
            objects_swept: report.swept,
            run_dirs_removed: report.run_dirs_removed,
            reclaimed_bytes: report.bytes_freed,
        };
        println!("{}", serde_json::to_string(&out).expect("serialize prune"));
    } else if dry_run {
        // No --force: preview only (mirrors gc's confirmation parity). gc's
        // dry-run reports the reclaimable object COUNT but not bytes (bytes are
        // only summed during a real sweep), so we report the count and direct
        // the user to --force for the actual reclaimed-byte figure.
        println!(
            "would reclaim {} objects, {} run dirs — pass --force",
            report.swept, report.run_dirs_removed
        );
    } else {
        println!(
            "Deleted {} objects, {} run dirs",
            report.swept, report.run_dirs_removed
        );
        println!("Total reclaimed space: {}", human_size(report.bytes_freed));
    }
    0
}

// ── dispatch ──────────────────────────────────────────────────────────────────

/// Route a `lightr system <subcmd>` invocation.
pub fn run(subcmd: SystemCmd) -> i32 {
    match subcmd {
        SystemCmd::Df { json } => df(json),
        SystemCmd::Prune {
            force,
            min_age,
            json,
        } => prune(force, min_age, json),
    }
}

#[cfg(test)]
#[path = "system_tests.rs"]
mod tests;
