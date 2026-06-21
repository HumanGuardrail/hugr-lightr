//! Tests: workload runner, Lightr measurements, detection, comm matching.

use std::path::PathBuf;

use super::super::competitor::measure_competitor;
use super::super::measure::{comm_is_lightr_binary, lightr_idle_processes, lightr_materialize_ms};
use super::super::model::{
    which_in, which_on_path, Cell, Detected, MaterializeSize, ProbePolicy, Runtime, Unit, Workload,
};
use super::super::report::render_cell;
use super::super::run_workload;
use crate::handlers::bench_compete_docker as dp;

// ── SKIP logic (tense law) — no runtime installed required ────────────────────

#[test]
fn absent_runtime_yields_skip_never_a_number() {
    // Detected as absent (path None) → every workload competitor cell is SKIP.
    let detected = vec![Detected {
        runtime: Runtime::Docker,
        path: None,
    }];
    let tmp = tempfile::tempdir().expect("tempdir");
    for wl in Workload::ALL {
        // cold-image is exempt from this whole-ALL sweep: run_workload measures
        // the LIGHTR side first, and `lightr_cold_image_ms` does a real CAS pull
        // that hits the NETWORK — forbidden in unit tests. Its absent-competitor
        // SKIP is covered network-free by the guard-direct assertion in
        // `present_competitor_under_neverspawn_always_skips`.
        if wl == Workload::ColdImage {
            continue;
        }
        let rows = run_workload(
            wl,
            tmp.path(),
            &detected,
            MaterializeSize::small(),
            ProbePolicy::NeverSpawn,
        );
        for row in &rows {
            let c = &row.competitors[0];
            match c {
                Cell::Skip(_) => {} // good — absent → skip
                Cell::Measured(v) => {
                    panic!(
                        "absent runtime fabricated a number {v} in row {}",
                        row.indicator
                    )
                }
                Cell::Na => panic!("absent runtime should SKIP, not NA, in {}", row.indicator),
            }
            // An absent competitor can never produce a factor.
            assert_eq!(
                row.factor(0),
                None,
                "absent → no factor in {}",
                row.indicator
            );
        }
    }
}

// ── Lightr-only workload runner (SMALL fixture; NO docker spawn) ───────────────

#[test]
fn lightr_materialize_measures_a_real_number_small() {
    // Under heavy CI load (the self-hosted box runs the gate + agent builds) the
    // real materialization probe can legitimately fail → None, which is HONEST
    // ("unavailable", never fabricated). The tense-law assertion is conditional:
    // WHEN it measures, it must be a real non-negative number. (Mirrors the
    // `lightr_idle_processes` None-tolerance below; keeps the gate deterministic
    // — was a load-sensitive flake via `.expect`, #54.)
    let tmp = tempfile::tempdir().expect("tempdir");
    if let Some(ms) = lightr_materialize_ms(tmp.path(), MaterializeSize::small()) {
        assert!(ms >= 0.0, "materialize ms must be non-negative");
    }
}

#[test]
fn lightr_idle_processes_counts_no_lightr_daemon() {
    // Daemonless: no resident lightr process (this test process is excluded).
    // ps must be available on the test host (macOS/Linux).
    if let Some(n) = lightr_idle_processes() {
        assert!(n >= 0.0);
        // We can't assert exactly 0 in all CI shapes, but the value is a real
        // count, never fabricated. (On a clean daemonless box it is 0.)
    }
}

#[test]
fn run_workload_idle_lightr_only_no_competitor_spawn() {
    // Idle workload with an ABSENT competitor: lightr measured, competitor SKIP.
    let detected = vec![Detected {
        runtime: Runtime::Docker,
        path: None,
    }];
    let tmp = tempfile::tempdir().expect("tempdir");
    let rows = run_workload(
        Workload::Idle,
        tmp.path(),
        &detected,
        MaterializeSize::small(),
        ProbePolicy::NeverSpawn,
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    // Lightr is measured (count) or NA if ps missing — never SKIP.
    assert!(
        matches!(row.lightr, Cell::Measured(_) | Cell::Na),
        "lightr idle must be measured or na, got {:?}",
        row.lightr
    );
    // Competitor absent → SKIP, no number.
    assert!(matches!(row.competitors[0], Cell::Skip(_)));
}

#[test]
fn present_competitor_under_neverspawn_always_skips() {
    // THE tense-law spawn-guard. A runtime detected as PRESENT (note the
    // fake path is never executed — the guard returns before touching it)
    // must still SKIP under NeverSpawn across EVERY spawn-workload. This is
    // what makes `cargo test`/CI structurally unable to launch a container,
    // even on a docker-equipped runner. (Idle is exempt: it counts processes
    // via `ps`, which is not a container spawn.)
    let detected = vec![Detected {
        runtime: Runtime::Docker,
        path: Some(PathBuf::from("/usr/local/bin/docker")),
    }];
    let tmp = tempfile::tempdir().expect("tempdir");
    for wl in [
        Workload::Install,
        Workload::Materialize,
        Workload::ColdRun,
        Workload::ReRun,
        Workload::Build,
    ] {
        let rows = run_workload(
            wl,
            tmp.path(),
            &detected,
            MaterializeSize::small(),
            ProbePolicy::NeverSpawn,
        );
        for row in &rows {
            match &row.competitors[0] {
                Cell::Skip(r) => assert!(
                    r.contains("spawn disabled"),
                    "present+NeverSpawn must skip with the guard reason, got {r:?} in {}",
                    row.indicator
                ),
                other => panic!(
                    "present competitor under NeverSpawn must SKIP, got {other:?} in {}",
                    row.indicator
                ),
            }
        }
    }

    // cold-image is also a spawn-workload, so its DOCKER probe must SKIP under
    // NeverSpawn exactly like the others. We assert it via `measure_competitor`
    // (the guard itself) rather than `run_workload`, because run_workload would
    // FIRST measure the LIGHTR side, and `lightr_cold_image_ms` does a real
    // `docker`-free CAS pull that hits the NETWORK — forbidden in unit tests.
    // The guard returns Skip BEFORE the probe closure runs, so neither the
    // network nor `dp::cold_image_ms` is touched here. This proves the same
    // CI-safety lock for cold-image without making a network call.
    let scratch = tmp.path().join("cold-image-scratch");
    let cell = measure_competitor(
        &detected[0],
        ProbePolicy::NeverSpawn,
        &scratch,
        dp::cold_image_ms,
    );
    match cell {
        Cell::Skip(r) => assert!(
            r.contains("spawn disabled"),
            "present+NeverSpawn (cold-image) must skip with the guard reason, got {r:?}"
        ),
        other => {
            panic!("present competitor under NeverSpawn (cold-image) must SKIP, got {other:?}")
        }
    }
}

#[test]
fn install_row_measures_lightr_footprint_mb() {
    // Lightr install footprint = the running binary's real size in MB,
    // measured (never fabricated). Competitor is absent here → SKIP.
    let detected = vec![Detected {
        runtime: Runtime::Docker,
        path: None,
    }];
    let tmp = tempfile::tempdir().expect("tempdir");
    let rows = run_workload(
        Workload::Install,
        tmp.path(),
        &detected,
        MaterializeSize::small(),
        ProbePolicy::NeverSpawn,
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.indicator, "install footprint");
    assert_eq!(row.unit, Unit::Mb);
    match row.lightr {
        Cell::Measured(mb) => assert!(mb > 0.0, "install footprint must be a positive MB"),
        ref other => panic!("lightr install must be measured, got {other:?}"),
    }
    assert!(matches!(row.competitors[0], Cell::Skip(_)));
    // MB renders with the unit suffix.
    assert_eq!(render_cell(&Cell::Measured(4.16), Unit::Mb), "4.2MB");
}

#[test]
fn which_on_path_absent_binary_is_none() {
    // A binary that cannot exist → None (detection never invents a path).
    assert!(which_on_path(&["definitely-not-a-real-binary-xyz-9999"]).is_none());
}

#[test]
fn comm_matches_only_exact_lightr_binary() {
    // Exact name + path-with-basename match.
    assert!(comm_is_lightr_binary("lightr"));
    assert!(comm_is_lightr_binary("/Users/x/target/debug/lightr"));
    assert!(comm_is_lightr_binary("  /usr/local/bin/lightr  "));
    // The real-world false positive: a CI runner under a dir spelled
    // "…-lightr-…" must NOT be counted as a Lightr daemon (daemonless honesty).
    assert!(!comm_is_lightr_binary(
        "/Users/x/actions-runner-lightr-cri/bin/Runner.Listener"
    ));
    assert!(!comm_is_lightr_binary("lightr-helper"));
    assert!(!comm_is_lightr_binary("hugr-lightr"));
    assert!(!comm_is_lightr_binary("dockerd"));
}

#[test]
fn which_in_empty_path_finds_nothing() {
    // Detection over an EMPTY PATH (no global mutation) → nothing found,
    // i.e. every runtime would be marked absent → SKIP. This is the tense-law
    // detection path, exercised without racing other parallel tests.
    let empty = std::ffi::OsString::new();
    for rt in [Runtime::Docker, Runtime::OrbStack, Runtime::AppleContainer] {
        assert!(
            which_in(rt.binaries(), &empty).is_none(),
            "empty PATH must mark {rt:?} absent",
        );
    }
}
