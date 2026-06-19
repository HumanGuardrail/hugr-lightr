//! Tests: parsing, cell/factor logic, skip/tense-law, table and JSON rendering.

use std::path::PathBuf;

use super::super::competitor::measure_competitor;
use super::super::model::{
    parse_runtimes, Cell, CmpRow, Detected, ProbePolicy, Runtime, Unit, Workload,
};
use super::super::report::{
    build_report_json, header_line, render_cell, render_factor, render_table,
};

// ── Parsing ──────────────────────────────────────────────────────────────────

#[test]
fn parse_runtimes_accepts_known_and_dedups() {
    let vs = vec![
        "docker".to_string(),
        "orbstack".to_string(),
        "container".to_string(),
        "docker".to_string(), // dup
    ];
    let got = parse_runtimes(&vs).expect("known runtimes parse");
    assert_eq!(
        got,
        vec![Runtime::Docker, Runtime::OrbStack, Runtime::AppleContainer]
    );
}

#[test]
fn parse_runtimes_accepts_orb_alias() {
    let got = parse_runtimes(&["orb".to_string()]).expect("orb alias parses");
    assert_eq!(got, vec![Runtime::OrbStack]);
}

#[test]
fn parse_runtimes_fails_closed_on_unknown() {
    let err = parse_runtimes(&["podman".to_string()]).unwrap_err();
    assert_eq!(err, "podman");
}

#[test]
fn workload_select_all_is_seven() {
    let got = Workload::select("all").expect("all parses");
    assert_eq!(got.len(), 7);
}

#[test]
fn workload_select_unknown_is_none() {
    assert!(Workload::select("bogus").is_none());
}

#[test]
fn workload_select_each_name() {
    assert_eq!(Workload::select("install"), Some(vec![Workload::Install]));
    assert_eq!(
        Workload::select("materialize"),
        Some(vec![Workload::Materialize])
    );
    assert_eq!(Workload::select("cold-run"), Some(vec![Workload::ColdRun]));
    assert_eq!(Workload::select("re-run"), Some(vec![Workload::ReRun]));
    assert_eq!(Workload::select("idle"), Some(vec![Workload::Idle]));
    assert_eq!(Workload::select("build"), Some(vec![Workload::Build]));
    assert_eq!(
        Workload::select("cold-image"),
        Some(vec![Workload::ColdImage])
    );
}

// ── Cell + factor logic ───────────────────────────────────────────────────────

#[test]
fn factor_only_when_both_measured() {
    let row = CmpRow {
        indicator: "x",
        unit: Unit::Millis,
        lightr: Cell::Measured(10.0),
        competitors: vec![
            Cell::Measured(100.0),        // factor 10x
            Cell::Skip("absent on PATH"), // no factor
            Cell::Na,                     // no factor
        ],
    };
    assert_eq!(row.factor(0), Some(10.0));
    assert_eq!(row.factor(1), None);
    assert_eq!(row.factor(2), None);
    assert_eq!(row.best_factor(), Some(10.0));
}

#[test]
fn factor_none_when_lightr_skipped() {
    let row = CmpRow {
        indicator: "x",
        unit: Unit::Millis,
        lightr: Cell::Skip("whatever"),
        competitors: vec![Cell::Measured(100.0)],
    };
    assert_eq!(row.factor(0), None);
    assert_eq!(row.best_factor(), None);
}

#[test]
fn factor_never_divides_by_zero_baseline() {
    // A zero lightr baseline (e.g. idle = 0 processes) must NOT fabricate an
    // infinite factor — it yields None.
    let row = CmpRow {
        indicator: "idle processes",
        unit: Unit::Count,
        lightr: Cell::Measured(0.0),
        competitors: vec![Cell::Measured(7.0)],
    };
    assert_eq!(row.factor(0), None);
    assert!(row.best_factor().is_none());
    // And it must render as "—", not "infx" or a number.
    assert_eq!(render_factor(&row), "—");
}

#[test]
fn best_factor_picks_the_max() {
    let row = CmpRow {
        indicator: "x",
        unit: Unit::Millis,
        lightr: Cell::Measured(2.0),
        competitors: vec![Cell::Measured(10.0), Cell::Measured(60.0)],
    };
    // 10/2 = 5x, 60/2 = 30x → best = 30x
    assert_eq!(row.best_factor(), Some(30.0));
}

// ── SKIP logic (tense law) — no runtime installed required ────────────────────

#[test]
fn skip_cell_renders_as_skip_word() {
    assert_eq!(
        render_cell(&Cell::Skip("absent on PATH"), Unit::Millis),
        "SKIP"
    );
    assert_eq!(render_cell(&Cell::Na, Unit::Count), "n/a");
    assert_eq!(render_cell(&Cell::Measured(12.34), Unit::Millis), "12.3ms");
    assert_eq!(render_cell(&Cell::Measured(3.0), Unit::Count), "3");
}

// ── Table formatter ───────────────────────────────────────────────────────────

#[test]
fn table_has_all_columns_and_header_caveat() {
    let detected = vec![
        Detected {
            runtime: Runtime::Docker,
            path: None,
        },
        Detected {
            runtime: Runtime::OrbStack,
            path: None,
        },
    ];
    let rows = vec![CmpRow {
        indicator: "idle processes",
        unit: Unit::Count,
        lightr: Cell::Measured(0.0),
        competitors: vec![Cell::Skip("absent on PATH"), Cell::Skip("absent on PATH")],
    }];
    let header = header_line(&detected);
    let table = render_table(&rows, &detected, &header);

    // Columns present.
    assert!(table.contains("indicator"));
    assert!(table.contains("lightr"));
    assert!(table.contains("docker"));
    assert!(table.contains("orbstack"));
    assert!(table.contains("factor"));
    // The honest header caveat.
    assert!(table.contains("Apple-Silicon headline binds when run on AS"));
    assert!(table.contains("numbers measured on THIS box"));
    // No competitor present → the loud note.
    assert!(table.contains("no competitor on PATH to compare against"));
    // No fabricated number — SKIP appears for both competitor cells.
    assert_eq!(table.matches("SKIP").count(), 2);
}

#[test]
fn header_line_lists_present_runtimes() {
    // All absent → "none".
    let detected = vec![Detected {
        runtime: Runtime::Docker,
        path: None,
    }];
    let h = header_line(&detected);
    assert!(h.contains("competitors present on PATH: none"));
}

// ── JSON shape ────────────────────────────────────────────────────────────────

#[test]
fn json_shape_is_honest_and_well_formed() {
    let detected = vec![Detected {
        runtime: Runtime::Docker,
        path: None,
    }];
    let rows = vec![CmpRow {
        indicator: "idle processes",
        unit: Unit::Count,
        lightr: Cell::Measured(0.0),
        competitors: vec![Cell::Skip("absent on PATH")],
    }];
    let report = build_report_json(&rows, &detected);
    let s = serde_json::to_string(&report).expect("serialize report");

    // Round-trips to a value with the expected structure.
    let v: serde_json::Value = serde_json::from_str(&s).expect("parse json");
    assert_eq!(v["machine"]["os"], std::env::consts::OS);
    assert_eq!(v["machine"]["arch"], std::env::consts::ARCH);
    // No competitor present → empty present list.
    assert!(v["machine"]["competitors_present"]
        .as_array()
        .expect("array")
        .is_empty());

    let row0 = &v["rows"][0];
    assert_eq!(row0["indicator"], "idle processes");
    assert_eq!(row0["unit"], "count");
    // Lightr measured 0.
    assert_eq!(row0["lightr"]["state"], "measured");
    assert_eq!(row0["lightr"]["value"], 0.0);
    // Competitor SKIP carries reason + NO value.
    let comp0 = &row0["competitors"][0];
    assert_eq!(comp0["runtime"], "docker");
    assert_eq!(comp0["state"], "skip");
    assert_eq!(comp0["reason"], "absent on PATH");
    assert!(comp0["value"].is_null());
    // No factor on the row (lightr=0 baseline AND competitor skipped).
    assert!(row0["factor"].is_null());
    assert!(comp0["factor"].is_null());
}

#[test]
fn json_emits_factor_when_both_measured() {
    let detected = vec![Detected {
        runtime: Runtime::Docker,
        path: Some(PathBuf::from("/usr/bin/docker")),
    }];
    let rows = vec![CmpRow {
        indicator: "idle processes",
        unit: Unit::Count,
        lightr: Cell::Measured(1.0),
        competitors: vec![Cell::Measured(9.0)],
    }];
    let report = build_report_json(&rows, &detected);
    let v: serde_json::Value =
        serde_json::from_value(serde_json::to_value(&report).expect("to_value"))
            .expect("from_value");
    assert_eq!(v["rows"][0]["factor"], 9.0);
    assert_eq!(v["rows"][0]["competitors"][0]["factor"], 9.0);
    assert_eq!(v["machine"]["competitors_present"][0], "docker");
}
