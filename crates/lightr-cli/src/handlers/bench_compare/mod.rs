//! `lightr bench-compare` handler — the head-to-head "humiliation" benchmark
//! (WP-C, build-spec-parity.md §5).
//!
//! Runs IDENTICAL workloads through Lightr and each competitor side-by-side and
//! prints a table (`indicator | lightr | docker | orbstack | container | factor`)
//! plus `--json`. The `factor` is `competitor / lightr` — the humiliation
//! multiple — printed ONLY where BOTH numbers were measured.
//!
//! ## Tense law (inviolable — ADR-0012, performance-bar.md)
//! NEVER print a number that was not measured. A competitor that is absent from
//! `$PATH` produces a printed **SKIP** cell, NEVER a fabricated number. Lightr is
//! ALWAYS measured (it is the subject); a competitor is measured only if present.
//! If NO competitor is on `$PATH`, Lightr's own numbers still print, with a clear
//! "no competitor on PATH to compare against" note.
//!
//! This is the marketing/proof harness — it has NO CI budget gate (that is the
//! plain `bench` verb). It draws its methodology from `bench.rs`: median-of-N
//! after a warmup, fixtures built in a tempdir with `LIGHTR_HOME` also a tempdir,
//! Lightr measured via the real code paths (in-process index ops + self-spawn).
//!
//! Honesty boundary on measuring competitors: spawning real Docker/OrbStack/Apple
//! `container` workloads (pull, run, build) is the harness's job at MARKETING time
//! on a real box. In CI/tests no container runtime is present, so the only path
//! exercised by tests is detection-and-skip. We measure for a competitor exactly
//! the surfaces we can run without fabricating anything; an op a present runtime
//! cannot perform (e.g. timed out) is itself a SKIP, not a guessed number.

pub mod competitor;
pub mod measure;
pub mod model;
pub mod report;

// Re-exports consumed by the sibling `bench_compete_docker` module via
// `use super::bench_compare::{…}` — the flat-module surface must be preserved.
pub(crate) use measure::{build_materialize_fixture, dur_ms, SAMPLES};
pub(crate) use model::MaterializeSize;

use std::path::Path;

use super::bench_compete_docker as dp;

use competitor::{competitor_idle_processes, measure_competitor};
use measure::{
    lightr_build_cached_ms, lightr_cold_image_ms, lightr_coldrun_ms, lightr_idle_processes,
    lightr_install_mb, lightr_materialize_ms, lightr_rerun_ms,
};
use model::{Cell, CmpRow, Detected, ProbePolicy, Unit, Workload};
use report::{build_report_json, header_line, render_table};

// ──────────────────────────────────────────────────────────────────────────────
// Workload runner: builds the rows for one workload
// ──────────────────────────────────────────────────────────────────────────────

/// Run ONE workload and produce its row(s). `home` is a per-invocation tempdir
/// used as `LIGHTR_HOME`; `detected` is the aligned competitor list; `size`
/// scopes the materialize fixture (small in tests, 1 GB for real runs); `policy`
/// is the spawn-guard (`Spawn` only from the real CLI; `NeverSpawn` in tests/CI).
///
/// TENSE LAW is enforced here. Lightr is always measured (it is the subject). A
/// competitor cell is `Skip` unless the runtime is present AND `policy == Spawn`
/// AND the probe returns a real measurement. Under `NeverSpawn` a present
/// competitor still SKIPs — so `cargo test`/CI never launch a container. A probe
/// that times out or whose setup fails is itself an honest SKIP, never a guess.
pub(crate) fn run_workload(
    wl: Workload,
    home: &Path,
    detected: &[Detected],
    size: MaterializeSize,
    policy: ProbePolicy,
) -> Vec<CmpRow> {
    // A path (not yet created) for any docker-side fixtures this workload needs;
    // the probe creates what it uses. Unused by Install/Idle (no spawn fixtures).
    let scratch = home.join(format!("docker-scratch-{wl:?}"));
    match wl {
        Workload::Install => {
            let lightr = match lightr_install_mb() {
                Some(mb) => Cell::Measured(mb),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| {
                    measure_competitor(d, policy, &scratch, |bin, _scr| {
                        dp::install_footprint_mb(bin)
                    })
                })
                .collect();
            vec![CmpRow {
                indicator: "install footprint",
                unit: Unit::Mb,
                lightr,
                competitors,
            }]
        }
        Workload::Materialize => {
            let lightr = match lightr_materialize_ms(home, size) {
                Some(ms) => Cell::Measured(ms),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| {
                    measure_competitor(d, policy, &scratch, |bin, scr| {
                        dp::materialize_ms(bin, scr, size)
                    })
                })
                .collect();
            vec![CmpRow {
                indicator: "materialize (CoW)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
        Workload::ColdRun => {
            let lightr = Cell::Measured(lightr_coldrun_ms(home));
            let competitors = detected
                .iter()
                .map(|d| measure_competitor(d, policy, &scratch, dp::cold_run_ms))
                .collect();
            vec![CmpRow {
                indicator: "cold-run (import+run)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
        Workload::ReRun => {
            let lightr = Cell::Measured(lightr_rerun_ms(home));
            let competitors = detected
                .iter()
                .map(|d| measure_competitor(d, policy, &scratch, dp::re_run_ms))
                .collect();
            vec![CmpRow {
                indicator: "re-run (memo hit)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
        Workload::Idle => {
            // The one head-to-head we can measure honestly with no container
            // spawn: process footprint of an idle install. Lightr = 0 (ps proves).
            let lightr = match lightr_idle_processes() {
                Some(n) => Cell::Measured(n),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| {
                    if !d.present() {
                        return Cell::Skip("absent on PATH");
                    }
                    // Present: count its resident daemon/VM processes via ps.
                    match competitor_idle_processes(d.runtime) {
                        Some(n) => Cell::Measured(n),
                        None => Cell::Skip("ps unavailable"),
                    }
                })
                .collect();
            vec![CmpRow {
                indicator: "idle processes",
                unit: Unit::Count,
                lightr,
                competitors,
            }]
        }
        Workload::Build => {
            let lightr = match lightr_build_cached_ms(home) {
                Some(ms) => Cell::Measured(ms),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| measure_competitor(d, policy, &scratch, dp::build_ms))
                .collect();
            vec![CmpRow {
                indicator: "build (memoized 2nd)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
        Workload::ColdImage => {
            let lightr = match lightr_cold_image_ms(home) {
                Some(ms) => Cell::Measured(ms),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| measure_competitor(d, policy, &scratch, dp::cold_image_ms))
                .collect();
            vec![CmpRow {
                indicator: "cold-image (CAS→CoW)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

/// Run the comparison. `vs` = runtimes to compare against, `workload` = which
/// workload(s) (`all` by default), `json` = machine-readable output.
pub fn run(vs: &[String], workload: &str, json: bool) -> i32 {
    // Parse the requested competitors (fail closed on an unknown token).
    let runtimes = match model::parse_runtimes(vs) {
        Ok(r) => r,
        Err(bad) => {
            eprintln!(
                "lightr: bench-compare: unknown runtime '{bad}' (expected docker, orbstack/orb, container)"
            );
            return 2;
        }
    };

    // Parse the requested workloads.
    let workloads = match Workload::select(workload) {
        Some(w) => w,
        None => {
            eprintln!(
                "lightr: bench-compare: unknown workload '{workload}' (expected all, materialize, cold-run, re-run, idle, build, cold-image)"
            );
            return 2;
        }
    };

    // Detect each requested runtime on PATH (present/absent only). A present
    // runtime is counted for the idle indicator (its daemon/VM shows in `ps`);
    // every other competitor surface here is an honest SKIP (we never spawn a
    // competitor container workload — tense law forbids fabricating its number).
    let detected = model::detect_all(&runtimes);

    // Per-invocation LIGHTR_HOME so Lightr's store/index are clean.
    let home_tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lightr: bench-compare: cannot create temp home: {e}");
            return 1;
        }
    };
    let home = home_tmp.path();

    // Real runs use the 1 GB materialize fixture (headline). Tests call the
    // internal runner directly with MaterializeSize::small().
    let size = MaterializeSize::real();

    // Run each workload, collecting rows. The real CLI entry is the ONLY caller
    // that authorizes spawning competitor containers (tense-law spawn-guard).
    let mut rows: Vec<CmpRow> = Vec::new();
    for wl in &workloads {
        rows.extend(run_workload(*wl, home, &detected, size, ProbePolicy::Spawn));
    }

    // Emit.
    if json {
        let report = build_report_json(&rows, &detected);
        match serde_json::to_string(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("lightr: bench-compare: serialize: {e}");
                return 1;
            }
        }
    } else {
        let header = header_line(&detected);
        print!("{}", render_table(&rows, &detected, &header));
    }

    0
}

#[cfg(test)]
mod tests;
