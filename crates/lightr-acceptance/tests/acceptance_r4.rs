//! A27–A30 per build-spec-r4.md §6.
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo build -q · cargo test -p lightr-acceptance --test acceptance_r4
//!
//! Dependency notes:
//!   A27 — requires R4-W1 (run --deep-memo flag + honest fallback). BLOCKED until W1 merges.
//!   A28 — requires R4-W2 (lightr schema subcommand). BLOCKED until W2 merges.
//!   A29 — requires R4-W2 (bench B9/B10/B11 indicators). BLOCKED until W2 merges.
//!   A30 — requires R4-W4 (docs/spec/parity-audit.md). BLOCKED until W4 merges.
//!
//! Tests are authored correctly per spec. They will fail (not panic-crash) until
//! the upstream WPs land. Do NOT weaken assertions.
//!
//! # run --json output note
//!
//! `lightr run --json` streams child stdout to stdout and emits a JSON summary
//! to STDERR prefixed `lightr-json: `. A28 parses that line from stderr.

#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

use common::lightr_cmd;
use std::fs;
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Create a minimal workspace with one file for run/snapshot exercises.
fn tiny_workspace(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/hello.txt"), b"hello lightr\n").unwrap();
    fs::write(root.join("README.md"), b"# test workspace\n").unwrap();
}

/// Extract the `lightr-json: {…}` line from stderr bytes and parse the object.
/// Returns None if the sentinel line is absent.
fn parse_run_json_from_stderr(stderr: &[u8]) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(stderr);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("lightr-json: ") {
            return serde_json::from_str(rest).ok();
        }
    }
    None
}

/// Parse a JSON array from stdout bytes.
fn parse_json_array(stdout: &[u8]) -> serde_json::Value {
    serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "expected JSON array on stdout; parse error: {e}\nraw: {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

/// Parse a JSON object from stdout bytes.
fn parse_json_object(stdout: &[u8]) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "expected JSON object on stdout; parse error: {e}\nraw: {}",
            String::from_utf8_lossy(stdout)
        )
    });
    assert!(v.is_object(), "expected JSON object, got: {v}");
    v
}

// ─────────────────────────────────────────────────────────────────────────────
// A27 — deep-memo honest fallback
//
// Spec (build-spec-r4.md §6 A27):
//   run --deep-memo -- /bin/echo hi → exit 0, stdout "hi".
//   On this host (R4 ships no shim): stderr contains "deep-memo" and "unavailable"
//   (honest fallback note). Second run: stderr contains "memo HIT".
//   Invariant: never crashes, exit 0 both runs.
//
// BLOCKED: requires R4-W1 (run --deep-memo flag). Fails until W1 merges.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a27_deep_memo_honest_fallback() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    tiny_workspace(ws.path());
    let ws_str = ws.path().to_str().unwrap();

    // ── First run ───────────────────────────────────────────────────────────
    let out1 = lightr_cmd(home.path())
        .args([
            "run",
            "--deep-memo",
            "--dir",
            ws_str,
            "--input",
            ws_str,
            "--",
            "/bin/echo",
            "hi",
        ])
        .output()
        .expect("first run must not fail to spawn");

    assert_eq!(
        out1.status.code().unwrap_or(-1),
        0,
        "first run --deep-memo must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out1.stderr)
    );

    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    assert!(
        stdout1.trim_end_matches('\n').trim_end_matches('\r') == "hi",
        "first run stdout must be \"hi\"; got: {stdout1:?}"
    );

    // On this host R4 ships no shim: deep-memo is unavailable.
    // Stderr MUST contain a fallback note with both "deep-memo" and "unavailable".
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    assert!(
        stderr1.contains("deep-memo"),
        "first run stderr must contain \"deep-memo\" (honest fallback note); got:\n{stderr1}"
    );
    assert!(
        stderr1.contains("unavailable"),
        "first run stderr must contain \"unavailable\" (honest fallback note); got:\n{stderr1}"
    );

    // ── Second run — whole-run fallback memoizes → HIT ──────────────────────
    let out2 = lightr_cmd(home.path())
        .args([
            "run",
            "--deep-memo",
            "--dir",
            ws_str,
            "--input",
            ws_str,
            "--",
            "/bin/echo",
            "hi",
        ])
        .output()
        .expect("second run must not fail to spawn");

    assert_eq!(
        out2.status.code().unwrap_or(-1),
        0,
        "second run --deep-memo must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out2.stderr)
    );

    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr2.contains("memo HIT"),
        "second run stderr must contain \"memo HIT\" (whole-run memoization fired); got:\n{stderr2}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A28 — schema validates
//
// Spec (build-spec-r4.md §6 A28):
//   For each verb in [snapshot, hydrate, status, run, diff, gc]:
//     1. `lightr schema --verb <v>` → parse JSON; read required[].
//     2. Run the verb with --json on a real workspace.
//     3. Assert every key in required[] is present in the real output object.
//   `lightr schema` (all) → parse JSON object; every entry has
//   "x-lightr-schema-version" == 1.
//
//   run --json: the JSON object is emitted to stderr prefixed "lightr-json: ".
//   gc --json:  JSON object on stdout.
//
// BLOCKED: requires R4-W2 (lightr schema subcommand). Fails until W2 merges.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a28_schema_validates() {
    let home = TempDir::new().unwrap();
    let ws = TempDir::new().unwrap();
    tiny_workspace(ws.path());
    let ws_str = ws.path().to_str().unwrap();

    // ── Snapshot a ws so hydrate/status/diff have a ref to work with ────────
    let snap_out = lightr_cmd(home.path())
        .args(["snapshot", "--name", "a28ref", "--dir", ws_str])
        .output()
        .expect("snapshot must not fail to spawn");
    assert_eq!(
        snap_out.status.code().unwrap_or(-1),
        0,
        "setup snapshot must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&snap_out.stderr)
    );

    // Second snapshot to give diff something to compare (2 lineage entries).
    let snap_out2 = lightr_cmd(home.path())
        .args(["snapshot", "--name", "a28ref", "--dir", ws_str])
        .output()
        .expect("second snapshot must not fail to spawn");
    assert_eq!(
        snap_out2.status.code().unwrap_or(-1),
        0,
        "second snapshot must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&snap_out2.stderr)
    );

    // ── Helper: get required[] from schema --verb <v> ────────────────────────
    let required_keys = |verb: &str| -> Vec<String> {
        let schema_out = lightr_cmd(home.path())
            .args(["schema", "--verb", verb])
            .output()
            .unwrap_or_else(|e| panic!("schema --verb {verb} failed to spawn: {e}"));
        assert_eq!(
            schema_out.status.code().unwrap_or(-1),
            0,
            "schema --verb {verb} must exit 0; stderr:\n{}",
            String::from_utf8_lossy(&schema_out.stderr)
        );
        let schema = parse_json_object(&schema_out.stdout);
        // required is an array of strings (may be absent → empty)
        schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };

    // ── snapshot --json ───────────────────────────────────────────────────────
    {
        let required = required_keys("snapshot");
        let out = lightr_cmd(home.path())
            .args(["snapshot", "--json", "--name", "a28snap", "--dir", ws_str])
            .output()
            .expect("snapshot --json must not fail to spawn");
        assert_eq!(out.status.code().unwrap_or(-1), 0);
        let obj = parse_json_object(&out.stdout);
        for key in &required {
            assert!(
                obj.get(key).is_some(),
                "snapshot --json output missing required key \"{key}\"; got: {obj}"
            );
        }
    }

    // ── hydrate --json ────────────────────────────────────────────────────────
    {
        let required = required_keys("hydrate");
        let dest = TempDir::new().unwrap();
        let out = lightr_cmd(home.path())
            .args([
                "hydrate",
                "--json",
                "--name",
                "a28ref",
                dest.path().to_str().unwrap(),
            ])
            .output()
            .expect("hydrate --json must not fail to spawn");
        assert_eq!(out.status.code().unwrap_or(-1), 0);
        let obj = parse_json_object(&out.stdout);
        for key in &required {
            assert!(
                obj.get(key).is_some(),
                "hydrate --json output missing required key \"{key}\"; got: {obj}"
            );
        }
    }

    // ── status --json ─────────────────────────────────────────────────────────
    {
        let required = required_keys("status");
        // Use a fresh ws that hasn't changed since the snapshot.
        let clean_ws = TempDir::new().unwrap();
        tiny_workspace(clean_ws.path());
        let clean_ws_home = TempDir::new().unwrap();
        // Snapshot the clean ws under a known ref.
        let snap = lightr_cmd(clean_ws_home.path())
            .args([
                "snapshot",
                "--name",
                "a28statusref",
                "--dir",
                clean_ws.path().to_str().unwrap(),
            ])
            .output()
            .expect("status setup snapshot must not fail to spawn");
        assert_eq!(snap.status.code().unwrap_or(-1), 0);
        let out = lightr_cmd(clean_ws_home.path())
            .args([
                "status",
                "--json",
                "--name",
                "a28statusref",
                "--dir",
                clean_ws.path().to_str().unwrap(),
            ])
            .output()
            .expect("status --json must not fail to spawn");
        // status exits 0 (clean) or 1 (dirty) — both are valid; we only care about JSON.
        let obj = parse_json_object(&out.stdout);
        for key in &required {
            assert!(
                obj.get(key).is_some(),
                "status --json output missing required key \"{key}\"; got: {obj}"
            );
        }
    }

    // ── run --json ────────────────────────────────────────────────────────────
    // run --json emits JSON to STDERR prefixed "lightr-json: ".
    {
        let required = required_keys("run");
        let out = lightr_cmd(home.path())
            .args([
                "run",
                "--json",
                "--dir",
                ws_str,
                "--input",
                ws_str,
                "--",
                "/bin/echo",
                "x",
            ])
            .output()
            .expect("run --json must not fail to spawn");
        assert_eq!(out.status.code().unwrap_or(-1), 0);
        let obj = parse_run_json_from_stderr(&out.stderr).unwrap_or_else(|| {
            panic!(
                "run --json must emit 'lightr-json: {{...}}' on stderr; stderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        for key in &required {
            assert!(
                obj.get(key).is_some(),
                "run --json stderr object missing required key \"{key}\"; got: {obj}"
            );
        }
    }

    // ── diff --json ───────────────────────────────────────────────────────────
    {
        let required = required_keys("diff");
        let out = lightr_cmd(home.path())
            .args(["diff", "--json", "--name", "a28ref"])
            .output()
            .expect("diff --json must not fail to spawn");
        // diff exits 0 if lineage exists; ignore dirty exit.
        let obj = parse_json_object(&out.stdout);
        for key in &required {
            assert!(
                obj.get(key).is_some(),
                "diff --json output missing required key \"{key}\"; got: {obj}"
            );
        }
    }

    // ── gc --json ─────────────────────────────────────────────────────────────
    {
        let required = required_keys("gc");
        let out = lightr_cmd(home.path())
            .args(["gc", "--json"])
            .output()
            .expect("gc --json must not fail to spawn");
        assert_eq!(out.status.code().unwrap_or(-1), 0);
        let obj = parse_json_object(&out.stdout);
        for key in &required {
            assert!(
                obj.get(key).is_some(),
                "gc --json output missing required key \"{key}\"; got: {obj}"
            );
        }
    }

    // ── schema (all) — every entry has x-lightr-schema-version == 1 ──────────
    {
        let all_out = lightr_cmd(home.path())
            .args(["schema"])
            .output()
            .expect("schema (all) must not fail to spawn");
        assert_eq!(
            all_out.status.code().unwrap_or(-1),
            0,
            "schema (all) must exit 0; stderr:\n{}",
            String::from_utf8_lossy(&all_out.stderr)
        );
        let all_obj = parse_json_object(&all_out.stdout);
        let map = all_obj.as_object().unwrap();
        assert!(
            !map.is_empty(),
            "schema (all) must return a non-empty object"
        );
        for (verb, schema) in map {
            let version = schema
                .get("x-lightr-schema-version")
                .and_then(|v| v.as_i64());
            assert_eq!(
                version,
                Some(1),
                "schema entry \"{verb}\" must have x-lightr-schema-version == 1; got: {schema}"
            );
        }
    }
}

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
