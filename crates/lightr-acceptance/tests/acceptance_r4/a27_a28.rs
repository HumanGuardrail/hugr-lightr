use super::helpers::*;
use crate::common::lightr_cmd;
use tempfile::TempDir;

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
