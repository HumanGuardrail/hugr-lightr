//! Table and JSON rendering for `bench-compare`.

use serde::Serialize;

use super::model::{Cell, CmpRow, Detected, Unit};

// ──────────────────────────────────────────────────────────────────────────────
// Rendering
// ──────────────────────────────────────────────────────────────────────────────

/// Render one cell for the human table.
pub(crate) fn render_cell(cell: &Cell, unit: Unit) -> String {
    match cell {
        Cell::Measured(v) => match unit {
            Unit::Count => format!("{}", v.round() as i64),
            _ => format!("{:.1}{}", v, unit.suffix()),
        },
        Cell::Skip(_) => "SKIP".to_string(),
        Cell::Na => "n/a".to_string(),
    }
}

/// Render the `factor` cell: the best (max) competitor/lightr multiple, or "—"
/// when no competitor was measured against a measured lightr.
pub(crate) fn render_factor(row: &CmpRow) -> String {
    match row.best_factor() {
        Some(f) => format!("{f:.1}x"),
        None => "—".to_string(),
    }
}

/// The honest header line (performance-bar.md tense law): machine class + which
/// runtimes were present + the Apple-Silicon binding caveat.
pub(crate) fn header_line(detected: &[Detected]) -> String {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    let present: Vec<&str> = detected
        .iter()
        .filter(|d| d.present())
        .map(|d| d.runtime.label())
        .collect();
    let present_str = if present.is_empty() {
        "none".to_string()
    } else {
        present.join(", ")
    };
    format!(
        "bench-compare on this box: {os}/{arch} | competitors present on PATH: {present_str} | \
numbers measured on THIS box; the Apple-Silicon headline binds when run on AS"
    )
}

/// Build the human-readable table as a `String` (testable without stdout).
pub(crate) fn render_table(rows: &[CmpRow], detected: &[Detected], header: &str) -> String {
    let mut s = String::new();
    s.push_str(header);
    s.push('\n');

    // Column header: indicator | lightr | <each runtime> | factor
    let mut head = format!("{:<22}  {:>12}", "indicator", "lightr");
    for d in detected {
        head.push_str(&format!("  {:>12}", d.runtime.label()));
    }
    head.push_str(&format!("  {:>8}", "factor"));
    s.push_str(&head);
    s.push('\n');

    let width = 22 + 2 + 12 + detected.len() * (2 + 12) + 2 + 8;
    s.push_str(&"-".repeat(width));
    s.push('\n');

    for row in rows {
        let mut line = format!(
            "{:<22}  {:>12}",
            row.indicator,
            render_cell(&row.lightr, row.unit)
        );
        for c in &row.competitors {
            line.push_str(&format!("  {:>12}", render_cell(c, row.unit)));
        }
        line.push_str(&format!("  {:>8}", render_factor(row)));
        s.push_str(&line);
        s.push('\n');
    }

    // If no competitor present at all, say so loudly (tense law).
    if !detected.iter().any(|d| d.present()) {
        s.push_str("note: no competitor on PATH to compare against — Lightr numbers shown alone\n");
    }
    s
}

// ──────────────────────────────────────────────────────────────────────────────
// JSON shape
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(crate) struct CellJson {
    /// "measured" | "skip" | "na"
    pub(crate) state: &'static str,
    /// present only when state == "measured"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<f64>,
    /// present only when state == "skip"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<&'static str>,
}

impl CellJson {
    pub(crate) fn from_cell(cell: &Cell) -> Self {
        match cell {
            Cell::Measured(v) => CellJson {
                state: "measured",
                value: Some(*v),
                reason: None,
            },
            Cell::Skip(r) => CellJson {
                state: "skip",
                value: None,
                reason: Some(r),
            },
            Cell::Na => CellJson {
                state: "na",
                value: None,
                reason: None,
            },
        }
    }
}

#[derive(Serialize)]
pub(crate) struct CompetitorCellJson {
    pub(crate) runtime: &'static str,
    #[serde(flatten)]
    pub(crate) cell: CellJson,
    /// competitor/lightr, only where both measured
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) factor: Option<f64>,
}

#[derive(Serialize)]
pub(crate) struct RowJson {
    pub(crate) indicator: &'static str,
    pub(crate) unit: &'static str,
    pub(crate) lightr: CellJson,
    pub(crate) competitors: Vec<CompetitorCellJson>,
    /// best (max) factor across measured competitors
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) factor: Option<f64>,
}

#[derive(Serialize)]
pub(crate) struct ReportJson {
    pub(crate) machine: MachineJson,
    pub(crate) rows: Vec<RowJson>,
}

#[derive(Serialize)]
pub(crate) struct MachineJson {
    pub(crate) os: &'static str,
    pub(crate) arch: &'static str,
    pub(crate) competitors_present: Vec<&'static str>,
    pub(crate) note: &'static str,
}

pub(crate) fn build_report_json(rows: &[CmpRow], detected: &[Detected]) -> ReportJson {
    let present: Vec<&'static str> = detected
        .iter()
        .filter(|d| d.present())
        .map(|d| d.runtime.label())
        .collect();

    let row_json = rows
        .iter()
        .map(|row| {
            let competitors = row
                .competitors
                .iter()
                .enumerate()
                .map(|(i, c)| CompetitorCellJson {
                    runtime: detected[i].runtime.label(),
                    cell: CellJson::from_cell(c),
                    factor: row.factor(i),
                })
                .collect();
            RowJson {
                indicator: row.indicator,
                unit: match row.unit {
                    Unit::Millis => "ms",
                    Unit::Count => "count",
                    Unit::Mb => "mb",
                },
                lightr: CellJson::from_cell(&row.lightr),
                competitors,
                factor: row.best_factor(),
            }
        })
        .collect();

    ReportJson {
        machine: MachineJson {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            competitors_present: present,
            note: "numbers measured on THIS box; Apple-Silicon headline binds when run on AS",
        },
        rows: row_json,
    }
}
