use super::helpers::*;
use crate::common::lightr_cmd;
use std::fs;
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// A29 — bench expanded (B9/B10/B11)
//
// Spec (build-spec-r4.md §6 A29):
//   `lightr bench --json` array includes entries for B9 (oci-import / "import"),
//   B10 (build-cached), B11 (compose-up latency / "compose").
//   Build-cached median <= build-cold median.
//
// BLOCKED: requires R4-W2 (bench B9/B10/B11 indicators). Fails until W2 merges.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a29_bench_expanded() {
    let home = TempDir::new().unwrap();

    let out = lightr_cmd(home.path())
        .args(["bench", "--json"])
        .output()
        .expect("bench --json must not fail to spawn");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "bench --json must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let arr = parse_json_array(&out.stdout);
    let rows = arr
        .as_array()
        .expect("bench --json must return a JSON array");

    // ── Helper: find a row whose indicator contains the given substring ───────
    let find_row = |pattern: &str| -> Option<&serde_json::Value> {
        rows.iter().find(|r| {
            r.get("indicator")
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase().contains(&pattern.to_lowercase()))
                .unwrap_or(false)
        })
    };

    // ── B9: oci-import ────────────────────────────────────────────────────────
    // Matches "B9" OR "import" in indicator id.
    let b9 = find_row("B9").or_else(|| find_row("import"));
    assert!(
        b9.is_some(),
        "bench --json must contain a B9/import indicator; got indicators: {:?}",
        rows.iter()
            .filter_map(|r| r.get("indicator").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
    );

    // ── B10: build-cached ─────────────────────────────────────────────────────
    let b10 = find_row("B10").or_else(|| find_row("build-cached"));
    assert!(
        b10.is_some(),
        "bench --json must contain a B10/build-cached indicator; got indicators: {:?}",
        rows.iter()
            .filter_map(|r| r.get("indicator").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
    );

    // ── B11: compose-up ───────────────────────────────────────────────────────
    let b11 = find_row("B11").or_else(|| find_row("compose"));
    assert!(
        b11.is_some(),
        "bench --json must contain a B11/compose indicator; got indicators: {:?}",
        rows.iter()
            .filter_map(|r| r.get("indicator").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
    );

    // ── Build-cached median <= build-cold median ──────────────────────────────
    // Find build-cached (B10) and build-cold rows.
    let b10_row = b10.unwrap();
    let b10_median = b10_row
        .get("median_ms")
        .and_then(|v| v.as_f64())
        .expect("B10 row must have median_ms");

    let b10_budget = b10_row
        .get("budget_ms")
        .and_then(|v| v.as_f64())
        .expect("B10 row must have budget_ms");

    // Find build-cold row (may be labelled "B10b", "build-cold", etc.)
    let b_cold = find_row("build-cold");
    if let Some(cold_row) = b_cold {
        let cold_median = cold_row
            .get("median_ms")
            .and_then(|v| v.as_f64())
            .expect("build-cold row must have median_ms");
        assert!(
            b10_median <= cold_median,
            "build-cached median ({b10_median:.1} ms) must be <= build-cold median ({cold_median:.1} ms) — incrementality claim"
        );
    } else {
        // If no separate cold row, B10 alone must be within its budget.
        assert!(
            b10_median < b10_budget,
            "build-cached median ({b10_median:.1} ms) must be < its budget ({b10_budget:.1} ms) when no cold row exists"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A30 — parity audit present
//
// Spec (build-spec-r4.md §6 A30):
//   docs/spec/parity-audit.md MUST exist.
//   Every F-\d{3} id found in docs/spec/feature-tree.md must appear in
//   docs/spec/parity-audit.md (no feature silently undocumented).
//
// Pure repo-file test (no binary invocation).
//
// BLOCKED: requires R4-W4 (parity-audit.md authored by the lead). Fails until
// W4 merges. This is the expected red state; do NOT remove this test.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a30_parity_audit_present() {
    // Locate the repo root relative to this test binary's manifest.
    // CARGO_MANIFEST_DIR is set by cargo to the acceptance crate root.
    // The spec docs live at <repo-root>/docs/spec/.
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo test"),
    );
    // crates/lightr-acceptance → go up 2 levels to repo root
    let repo_root = manifest_dir
        .parent() // crates/
        .and_then(|p| p.parent()) // repo root
        .expect("could not locate repo root from CARGO_MANIFEST_DIR");

    let feature_tree = repo_root.join("docs/spec/feature-tree.md");
    let parity_audit = repo_root.join("docs/spec/parity-audit.md");

    // ── Read feature-tree.md and extract all F-NNN ids ───────────────────────
    assert!(
        feature_tree.exists(),
        "docs/spec/feature-tree.md must exist at {}",
        feature_tree.display()
    );
    let ft_content = fs::read_to_string(&feature_tree)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", feature_tree.display()));

    // Regex-equivalent: collect all F-\d{3} tokens.
    let f_ids: std::collections::BTreeSet<String> = {
        let mut ids = std::collections::BTreeSet::new();
        let bytes = ft_content.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Look for 'F' followed by '-' followed by exactly 3 digits.
            if bytes[i] == b'F'
                && i + 4 < bytes.len()
                && bytes[i + 1] == b'-'
                && bytes[i + 2].is_ascii_digit()
                && bytes[i + 3].is_ascii_digit()
                && bytes[i + 4].is_ascii_digit()
            {
                // Ensure it's a word boundary on both sides (not part of a longer token).
                let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
                let after_ok = i + 5 >= bytes.len() || !bytes[i + 5].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    let id = std::str::from_utf8(&bytes[i..i + 5]).unwrap().to_string();
                    ids.insert(id);
                }
            }
            i += 1;
        }
        ids
    };

    assert!(
        !f_ids.is_empty(),
        "feature-tree.md must contain at least one F-NNN id"
    );

    // ── parity-audit.md must exist (W4 deliverable) ───────────────────────────
    assert!(
        parity_audit.exists(),
        "docs/spec/parity-audit.md must exist (authored in R4-W4); \
         this test is correctly RED until W4 merges. \
         Missing file: {}",
        parity_audit.display()
    );

    let audit_content = fs::read_to_string(&parity_audit)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", parity_audit.display()));

    // ── Every F-NNN id from feature-tree must appear in parity-audit ─────────
    let mut missing: Vec<String> = Vec::new();
    for id in &f_ids {
        if !audit_content.contains(id.as_str()) {
            missing.push(id.clone());
        }
    }

    assert!(
        missing.is_empty(),
        "parity-audit.md is missing coverage for {} feature(s): {:?}\n\
         Every F-NNN id in feature-tree.md must appear in parity-audit.md.",
        missing.len(),
        missing
    );
}
