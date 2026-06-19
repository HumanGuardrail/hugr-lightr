//! A22, A22b (dir-copy + explain-report), A23 acceptance tests.

use super::helpers::*;
use crate::common::lightr_cmd;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// A22 — build memoizes
//
// NOTE: RUN runs via the native engine (no isolation on this box), which means
// it CAN write to an absolute path outside the build context. COUNTER_PATH
// lives in a separate tempdir; this is intentional.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a22_build_memoizes() {
    let home = TempDir::new().unwrap();
    let ctx = TempDir::new().unwrap();
    let counter_dir = TempDir::new().unwrap();
    let counter_path = counter_dir.path().join("counter.txt");

    // Write context file.
    let data_path = ctx.path().join("data.txt");
    fs::write(&data_path, b"v1").unwrap();

    // Write Dockerfile. COUNTER_PATH is a real absolute path OUTSIDE ctx.
    // The RUN step writes to it; because the native engine has no isolation,
    // it can reach any path on the host.
    let counter_str = counter_path.to_string_lossy();
    let dockerfile_content = format!(
        "FROM scratch\n\
         COPY data.txt /src/data.txt\n\
         RUN /bin/sh -c 'echo built >> {counter_str} && echo ran > built.txt'\n"
    );
    let df_path = ctx.path().join("Dockerfile");
    fs::write(&df_path, &dockerfile_content).unwrap();

    // ── first build: expect steps=3, cached=0 ────────────────────────────────
    let out1 = lightr_cmd(home.path())
        .args(["build", "-t", "@t/b", ctx.path().to_str().unwrap()])
        .output()
        .expect("build must not fail to spawn");
    assert_eq!(
        out1.status.code().unwrap_or(-1),
        0,
        "first build must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let (steps1, cached1) = parse_build_report(&out1.stdout);
    assert_eq!(
        steps1, 3,
        "first build: expected steps=3, got steps={steps1}"
    );
    assert_eq!(
        cached1, 0,
        "first build: expected cached=0 (no cache yet), got cached={cached1}"
    );

    // Counter has exactly 1 line after first build.
    let counter_lines_1 = if counter_path.exists() {
        fs::read_to_string(&counter_path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    } else {
        0
    };
    assert_eq!(
        counter_lines_1, 1,
        "after first build, COUNTER must have exactly 1 line (RUN ran once); got {counter_lines_1}"
    );

    // ── second build (unchanged): cached == steps (==3) ─────────────────────
    let out2 = lightr_cmd(home.path())
        .args(["build", "-t", "@t/b", ctx.path().to_str().unwrap()])
        .output()
        .expect("second build must not fail to spawn");
    assert_eq!(
        out2.status.code().unwrap_or(-1),
        0,
        "second build must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let (steps2, cached2) = parse_build_report(&out2.stdout);
    assert_eq!(
        steps2, 3,
        "second build: expected steps=3, got steps={steps2}"
    );
    assert_eq!(
        cached2, steps2,
        "second build: all steps must be cache hits (cached==steps=={steps2}); got cached={cached2}"
    );

    // Counter still has exactly 1 line (RUN memoized, did NOT re-execute).
    let counter_lines_2 = fs::read_to_string(&counter_path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(
        counter_lines_2, 1,
        "after second build (cached), COUNTER must still have 1 line; got {counter_lines_2}"
    );

    // ── modify ctx/data.txt; third build: RUN re-executes ───────────────────
    fs::write(&data_path, b"v2-changed").unwrap();

    let out3 = lightr_cmd(home.path())
        .args(["build", "-t", "@t/b", ctx.path().to_str().unwrap()])
        .output()
        .expect("third build must not fail to spawn");
    assert_eq!(
        out3.status.code().unwrap_or(-1),
        0,
        "third build must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out3.stderr)
    );
    let (steps3, cached3) = parse_build_report(&out3.stdout);
    assert_eq!(
        steps3, 3,
        "third build: expected steps=3, got steps={steps3}"
    );
    // FROM scratch is cached (same empty base); COPY+RUN re-run (file changed).
    assert!(
        cached3 >= 1,
        "third build: FROM scratch must still be cached (cached>=1); got cached={cached3}"
    );
    assert!(
        cached3 < steps3,
        "third build: COPY+RUN must re-run after data.txt changed (cached<steps); got cached={cached3} steps={steps3}"
    );

    // Counter now has exactly 2 lines (RUN re-ran once more).
    let counter_lines_3 = fs::read_to_string(&counter_path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(
        counter_lines_3, 2,
        "after third build (RUN re-ran), COUNTER must have 2 lines; got {counter_lines_3}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A22b — directory COPY cache invalidation (final-critic regression)
//
// `COPY src /app` must invalidate the build cache when a file INSIDE the
// copied directory changes — the bug the narrowed file-COPY A22 hid.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn a22b_dir_copy_invalidates_on_nested_change() {
    let home = TempDir::new().unwrap();
    let ctx = TempDir::new().unwrap();
    let counter_dir = TempDir::new().unwrap();
    let counter = counter_dir.path().join("counter.txt");

    std::fs::create_dir_all(ctx.path().join("src/nested")).unwrap();
    std::fs::write(ctx.path().join("src/nested/b.txt"), b"one").unwrap();
    let dockerfile = format!(
        "FROM scratch\nCOPY src /app\nRUN /bin/sh -c 'echo built >> {}'\n",
        counter.to_str().unwrap()
    );
    std::fs::write(ctx.path().join("Dockerfile"), dockerfile).unwrap();

    let build = |home: &Path| {
        lightr_cmd(home)
            .current_dir(ctx.path())
            .args(["build", "-t", "@t/dircopy", "."])
            .output()
            .expect("build runs")
    };

    // first build → RUN executes (counter = 1)
    assert_eq!(build(home.path()).status.code(), Some(0));
    // second build, unchanged → fully cached (counter stays 1)
    assert_eq!(build(home.path()).status.code(), Some(0));
    let lines_after_cached = std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(
        lines_after_cached, 1,
        "unchanged dir-COPY build must be cached (counter==1); got {lines_after_cached}"
    );

    // change a NESTED file → the COPY step key must change → RUN re-executes
    std::fs::write(ctx.path().join("src/nested/b.txt"), b"two").unwrap();
    assert_eq!(build(home.path()).status.code(), Some(0));
    let lines_after_change = std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(
        lines_after_change, 2,
        "nested file change must bust the dir-COPY cache (counter==2); got {lines_after_change}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A23 — build hydrate
//
// After A22's third build the ref @t/b is the final state (data.txt=v2-changed,
// /built.txt="ran"). Run a fresh build in its own home, then hydrate and assert:
//   - dest/src/data.txt exists and has the last data.txt content
//   - dest/built.txt exists with content "ran\n" (from `echo ran > built.txt`)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a23_build_hydrate() {
    let home = TempDir::new().unwrap();
    let ctx = TempDir::new().unwrap();
    let counter_dir = TempDir::new().unwrap();
    let counter_path = counter_dir.path().join("counter_a23.txt");

    let data_content = b"a23-data";
    fs::write(ctx.path().join("data.txt"), data_content).unwrap();

    let counter_str = counter_path.to_string_lossy();
    // RUN writes to counter (outside ctx) AND creates /built.txt inside the tree.
    let dockerfile_content = format!(
        "FROM scratch\n\
         COPY data.txt /src/data.txt\n\
         RUN /bin/sh -c 'echo built >> {counter_str} && echo ran > built.txt'\n"
    );
    let df_path = ctx.path().join("Dockerfile");
    fs::write(&df_path, &dockerfile_content).unwrap();

    // Build the image.
    let build_out = lightr_cmd(home.path())
        .args(["build", "-t", "@t/b", ctx.path().to_str().unwrap()])
        .output()
        .expect("build must not fail to spawn");
    assert_eq!(
        build_out.status.code().unwrap_or(-1),
        0,
        "build must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&build_out.stderr)
    );

    // Hydrate the built ref into a destination directory.
    let dest = TempDir::new().unwrap();
    let hydrate_out = lightr_cmd(home.path())
        .args(["hydrate", dest.path().to_str().unwrap(), "--name", "@t/b"])
        .output()
        .expect("hydrate must not fail to spawn");
    assert_eq!(
        hydrate_out.status.code().unwrap_or(-1),
        0,
        "hydrate must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&hydrate_out.stderr)
    );

    // dest/src/data.txt must exist with the original content.
    let hydrated_data = dest.path().join("src/data.txt");
    assert!(
        hydrated_data.exists(),
        "dest/src/data.txt must exist after hydrate"
    );
    assert_eq!(
        fs::read(&hydrated_data).unwrap(),
        data_content,
        "dest/src/data.txt content must match the COPY'd file"
    );

    // dest/built.txt must exist with content "ran\n".
    let hydrated_built = dest.path().join("built.txt");
    assert!(
        hydrated_built.exists(),
        "dest/built.txt must exist (created by RUN `echo ran > built.txt`)"
    );
    let built_content = fs::read_to_string(&hydrated_built).unwrap();
    assert_eq!(
        built_content.trim(),
        "ran",
        "dest/built.txt must contain 'ran' (echo ran > built.txt); got: {:?}",
        built_content
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A22b — verify the `--explain` flag alone also shows the steps/cached line
//
// Quick sanity: `build --explain` must still emit steps=N cached=N on stdout.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a22b_build_explain_emits_report() {
    let home = TempDir::new().unwrap();
    let ctx = TempDir::new().unwrap();
    fs::write(ctx.path().join("f.txt"), b"x").unwrap();
    fs::write(
        ctx.path().join("Dockerfile"),
        "FROM scratch\nCOPY f.txt /f.txt\n",
    )
    .unwrap();

    let out = lightr_cmd(home.path())
        .args([
            "build",
            "-t",
            "@t/expl",
            "--explain",
            ctx.path().to_str().unwrap(),
        ])
        .output()
        .expect("build --explain must not fail to spawn");

    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "build --explain must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Must parse steps= and cached= from stdout.
    let (steps, _cached) = parse_build_report(&out.stdout);
    assert_eq!(steps, 2, "FROM+COPY = 2 steps; got steps={steps}");
}
