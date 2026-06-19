//! `lightr bench` handler — build-spec v2 §9.
//!
//! Builds a fixture tree (2k files × 1KiB + 1×8MiB) in a tempdir with
//! LIGHTR_HOME also tempdir; measures with std::time::Instant medians-of-5
//! after 1 warmup.
//!
//! Budgets compiled in:
//!   B1  version overhead:    5 ms   (spawn-measured → ×3 in --check CI)
//!   B2  memo-hit run:        50 ms machine-class (spawn-measured → ×3 in --check)
//!   B4  replay:              35 ms machine-class (views/AS unlock the ~10 ms target)
//!   B6  status warm-index:   500 ms
//!   B3′ hydrate:             5000 ms
//!   B5a snapshot cold:       2500 ms
//!   B5b snapshot warm:       500 ms
//!   B12 microwave-floor:     10 000 ms (vz cold run; SKIP when vz unavailable)
//!
//! --check: exit 1 if any over budget.
//! --vs-docker: compare docker version overhead (2s timeout); skip if absent.
//! --json: array of {indicator,median_ms,budget_ms,pass} or {indicator,skip,reason}.

use lightr_store::Store;
use serde::Serialize;

mod fixture;
mod measure;
#[cfg(test)]
mod tests;

// ──────────────────────────────────────────────────────────────────────────────
// Budgets (frozen)
// ──────────────────────────────────────────────────────────────────────────────

const BUDGET_VERSION_MS: u64 = 5;
// Machine-class law (spec §9, S4 + first bench run, Intel i7 dev box):
// an end-to-end memo hit must re-validate inputs = warm stat-walk of the
// fixture (~45 ms k files here). The ~10 ms whitepaper target binds to
// the R2 views layer (mutation-tracked, no walk) + Apple Silicon.
const BUDGET_HIT_RUN_MS: u64 = 50;
const BUDGET_REPLAY_MS: u64 = 35;
const BUDGET_STATUS_WARM_MS: u64 = 500;
const BUDGET_HYDRATE_MS: u64 = 5_000;
const BUDGET_SNAPSHOT_COLD_MS: u64 = 2_500;
const BUDGET_SNAPSHOT_WARM_MS: u64 = 500;
// R4 §3 bench expansion (B9–B11) — generous machine-class budgets
const BUDGET_OCI_IMPORT_MS: u64 = 2_000;
const BUDGET_BUILD_COLD_MS: u64 = 5_000;
const BUDGET_BUILD_CACHED_MS: u64 = 2_000;
const BUDGET_COMPOSE_UP_MS: u64 = 3_000;
// F-603 microwave-floor: cold vz run budget. Generous: boot + guest exec + teardown.
// This row only runs when vz + linux pack are available; when absent it is a SKIP
// (never measured, never fails --check).
const BUDGET_MICROWAVE_FLOOR_MS: u64 = 10_000;

/// Spawn-measured indicators get ×3 margin in --check (debug/CI noise).
const SPAWN_MARGIN: u64 = 3;

// ──────────────────────────────────────────────────────────────────────────────
// Row types
// ──────────────────────────────────────────────────────────────────────────────

struct Row {
    indicator: &'static str,
    median_ms: f64,
    budget_ms: u64,
    /// effective budget for --check (may be ×3 for spawn-measured)
    check_budget_ms: u64,
    pass: bool,
}

impl Row {
    fn new(
        indicator: &'static str,
        dur: std::time::Duration,
        budget_ms: u64,
        spawn_measured: bool,
    ) -> Self {
        let median_ms = dur.as_secs_f64() * 1000.0;
        let check_budget_ms = if spawn_measured {
            budget_ms * SPAWN_MARGIN
        } else {
            budget_ms
        };
        let pass = median_ms <= check_budget_ms as f64;
        Row {
            indicator,
            median_ms,
            budget_ms,
            check_budget_ms,
            pass,
        }
    }
}

/// A bench row that was deliberately skipped because a prerequisite was absent
/// (e.g. vz engine + linux pack not available). A SKIP row:
///   - never fails --check (absence of a prerequisite is not a budget overflow),
///   - appears in --json as `{"indicator":…,"skip":true,"reason":…}`,
///   - appears in the human table as "SKIP (<reason>)".
struct SkipRow {
    indicator: &'static str,
    reason: String,
}

/// The union of a measured row and a skipped row, so both can live in one
/// `Vec` and be iterated uniformly for output.
enum BenchRow {
    Measured(Row),
    Skipped(SkipRow),
}

impl BenchRow {
    /// True iff this is a measured row that exceeded its budget.
    /// A SKIP row is never a failure.
    fn is_fail(&self) -> bool {
        match self {
            BenchRow::Measured(r) => !r.pass,
            BenchRow::Skipped(_) => false,
        }
    }
}

#[derive(Serialize)]
struct RowJson {
    indicator: String,
    skip: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    median_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pass: Option<bool>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

pub fn run(vs_docker: bool, check: bool, json: bool) -> i32 {
    // Temp dirs for fixture + store.
    let fixture_tmp = tempfile::tempdir().expect("fixture tempdir");
    let home_tmp = tempfile::tempdir().expect("home tempdir");

    let fixture_root = fixture_tmp.path().to_path_buf();
    let lightr_home = home_tmp.path().to_path_buf();

    // Override LIGHTR_HOME for subprocess spawns via env.
    // (The lightr_store::Store::default_root() reads $LIGHTR_HOME at call time.)
    std::env::set_var("LIGHTR_HOME", &lightr_home);

    // Build fixture.
    fixture::build_fixture(&fixture_root).expect("build fixture");

    let store_root = lightr_home.join("store");
    let open_store = || Store::open(&store_root).expect("open store");

    let mut rows: Vec<BenchRow> = Vec::new();

    // ── B1 ────────────────────────────────────────────────────────────────
    rows.push(measure::b1_version());

    // ── B5a + B5b ─────────────────────────────────────────────────────────
    rows.extend(measure::b5a_b5b_snapshot(
        &fixture_root,
        &store_root,
        &lightr_home,
        &open_store,
    ));

    // ── B6 ────────────────────────────────────────────────────────────────
    rows.push(measure::b6_status(&fixture_root, &open_store));

    // ── B3′ ───────────────────────────────────────────────────────────────
    rows.push(measure::b3_hydrate(&fixture_root, &open_store));

    // ── B2/B4 ─────────────────────────────────────────────────────────────
    rows.extend(measure::b2_b4_run_memo(&fixture_root, &store_root));

    // ── B9 ────────────────────────────────────────────────────────────────
    rows.push(measure::b9_oci_import());

    // ── B10a/B10 ──────────────────────────────────────────────────────────
    rows.extend(measure::b10_build());

    // ── B11 ───────────────────────────────────────────────────────────────
    rows.push(measure::b11_compose());

    // ── B12 ───────────────────────────────────────────────────────────────
    rows.push(measure::b12_microwave());

    // ── --vs-docker ────────────────────────────────────────────────────────
    let docker_line: Option<String> = if vs_docker {
        fixture::check_docker()
    } else {
        None
    };

    // ── Output ─────────────────────────────────────────────────────────────
    let any_fail = rows.iter().any(|r| r.is_fail());

    if json {
        let arr: Vec<RowJson> = rows
            .iter()
            .map(|row| match row {
                BenchRow::Measured(r) => RowJson {
                    indicator: r.indicator.to_string(),
                    skip: false,
                    reason: None,
                    median_ms: Some(r.median_ms),
                    budget_ms: Some(r.budget_ms),
                    pass: Some(r.pass),
                },
                BenchRow::Skipped(s) => RowJson {
                    indicator: s.indicator.to_string(),
                    skip: true,
                    reason: Some(s.reason.clone()),
                    median_ms: None,
                    budget_ms: None,
                    pass: None,
                },
            })
            .collect();
        println!("{}", serde_json::to_string(&arr).expect("serialize bench"));
    } else {
        println!(
            "{:<22}  {:>10}  {:>10}  verdict",
            "indicator", "median", "budget"
        );
        println!("{}", "-".repeat(58));
        for row in &rows {
            match row {
                BenchRow::Measured(r) => {
                    println!(
                        "{:<22}  {:>9.1}ms  {:>9}ms  {}",
                        r.indicator,
                        r.median_ms,
                        r.check_budget_ms,
                        if r.pass { "PASS" } else { "FAIL" }
                    );
                }
                BenchRow::Skipped(s) => {
                    println!("{:<22}  SKIP  {}", s.indicator, s.reason);
                }
            }
        }
        if let Some(line) = &docker_line {
            println!("{line}");
        }
    }

    if check && any_fail {
        1
    } else {
        0
    }
}
