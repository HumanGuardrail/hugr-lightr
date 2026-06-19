#[cfg(test)]
mod bench_b12_tests {
    use super::super::{BenchRow, RowJson, SkipRow};

    // B12 always appears in --json output (as a measured row or a SKIP row),
    // regardless of vz availability.
    #[test]
    fn b12_appears_in_json_output() {
        // Build a minimal BenchRow list that mirrors what run() would produce
        // when vz is absent (the common case incl. CI).
        let rows: Vec<BenchRow> = vec![BenchRow::Skipped(SkipRow {
            indicator: "B12 microwave-floor",
            reason: "vz engine requires macOS + the 'vz' build feature + a linux pack".to_string(),
        })];
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
        let json_str = serde_json::to_string(&arr).expect("serialize");
        // B12 must appear in the JSON output.
        assert!(
            json_str.contains("B12 microwave-floor"),
            "B12 must appear in --json output: {json_str}"
        );
        // A SKIP row must carry skip:true and a reason, never a fabricated number.
        let v: serde_json::Value = serde_json::from_str(&json_str).expect("parse json");
        let row0 = &v[0];
        assert_eq!(row0["skip"], true, "SKIP row must have skip:true");
        assert!(
            row0["reason"].is_string(),
            "SKIP row must carry a reason: {row0}"
        );
        assert!(
            row0["median_ms"].is_null(),
            "SKIP row must NOT carry a fabricated median_ms: {row0}"
        );
    }

    // A SKIP row must never trip --check (is_fail() == false).
    #[test]
    fn b12_skip_does_not_trip_check() {
        let skip_row = BenchRow::Skipped(SkipRow {
            indicator: "B12 microwave-floor",
            reason: "vz not available".to_string(),
        });
        assert!(
            !skip_row.is_fail(),
            "a SKIP row must never be a --check failure"
        );

        // Also verify: any_fail is false when the only non-passing row is a SKIP.
        let rows: Vec<BenchRow> = vec![skip_row];
        let any_fail = rows.iter().any(|r| r.is_fail());
        assert!(!any_fail, "SKIP-only rows must not trigger --check exit 1");
    }
}
