//! WP-C end-to-end tests: `build --target <stage>` selects an intermediate
//! stage; an unknown target errors; `FROM --platform` validates against the
//! base image's actual platform and folds into the memo key (two platforms ⇒
//! distinct keys). Exercised through the full `build`/`build_target` loop.
//!
//! Each test owns its tempdirs + store and never MUTATES process-global state,
//! but `build_target()`/`hydrate` READ the process-global `LIGHTR_HOME`, so the
//! `build_target_df`/`hydrate` helpers hold the crate-wide shared read lock
//! (`build::LIGHTR_HOME_ENV_LOCK`) to exclude the setter tests
//! (exec_tests/up_tests) while they run. Readers still parallelize among
//! themselves.
use super::*;
use lightr_store::ImageManifestRecord;
use tempfile::TempDir;

struct Fix {
    _ctx: TempDir,
    _store_tmp: TempDir,
    store: Store,
    ctx_path: std::path::PathBuf,
    store_tmp_path: std::path::PathBuf,
}

fn fix() -> Fix {
    let _ctx = TempDir::new().unwrap();
    let _store_tmp = TempDir::new().unwrap();
    let store = Store::open(_store_tmp.path().join("store")).unwrap();
    let ctx_path = _ctx.path().to_path_buf();
    let store_tmp_path = _store_tmp.path().to_path_buf();
    Fix {
        _ctx,
        _store_tmp,
        store,
        ctx_path,
        store_tmp_path,
    }
}

fn build_target_df(
    f: &Fix,
    name: &str,
    df_body: &str,
    target: Option<&str>,
) -> Result<BuildReport> {
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(&df_path, df_body).unwrap();
    // build_target() READs the process-global LIGHTR_HOME; hold the crate-wide
    // shared read lock so a concurrent setter cannot flip the home mid-build.
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
    build_target(
        &f.ctx_path,
        &df_path,
        name,
        lightr_engine::EngineKind::Native,
        &f.store,
        &[],
        target,
    )
}

fn hydrate(f: &Fix, name: &str, tag: &str) -> std::path::PathBuf {
    let dest = f.store_tmp_path.join(format!("hydrated-{tag}"));
    // hydrate READs the process-global LIGHTR_HOME; hold the shared read lock.
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
    lightr_index::hydrate(&dest, &f.store, name).unwrap();
    dest
}

/// Run a build expected to FAIL, returning the formatted error (BuildReport does
/// not impl Debug, so `unwrap_err` is unusable — `match` instead).
fn expect_err(r: Result<BuildReport>) -> String {
    match r {
        Ok(_) => panic!("expected build to fail, but it succeeded"),
        Err(e) => format!("{e}"),
    }
}

// ── build --target ──────────────────────────────────────────────────────────

#[test]
fn target_intermediate_outputs_that_stage_not_final() {
    // `--target builder` must output the INTERMEDIATE stage's tree (which has
    // /out/app.txt) — NOT the final stage's (which copies it to /app.txt). The
    // final stage's COPY --from must NOT run at all.
    let f = fix();
    std::fs::write(f.ctx_path.join("app.txt"), b"artifact").unwrap();
    let df = "FROM scratch AS builder\n\
              COPY app.txt /out/app.txt\n\
              FROM scratch\n\
              COPY --from=builder /out/app.txt /app.txt\n";
    let report = build_target_df(&f, "wpc-target", df, Some("builder")).unwrap();
    // Only the 2 steps of the builder stage ran (FROM + COPY), not the final 2.
    assert_eq!(
        report.steps, 2,
        "only the target stage's steps run: {}",
        report.steps
    );
    let dest = hydrate(&f, "wpc-target", "target");
    assert_eq!(
        std::fs::read_to_string(dest.join("out/app.txt")).unwrap(),
        "artifact",
        "the target (builder) stage's /out/app.txt is the output"
    );
    assert!(
        !dest.join("app.txt").exists(),
        "the FINAL stage's /app.txt must NOT exist — its COPY --from never ran"
    );
}

#[test]
fn target_is_case_insensitive() {
    // Docker matches stage names case-insensitively; `--target BUILDER` selects
    // the `AS builder` stage.
    let f = fix();
    std::fs::write(f.ctx_path.join("a.txt"), b"x").unwrap();
    let df = "FROM scratch AS builder\n\
              COPY a.txt /out/a.txt\n\
              FROM scratch\n\
              COPY --from=builder /out/a.txt /a.txt\n";
    let report = build_target_df(&f, "wpc-ci", df, Some("BUILDER")).unwrap();
    assert_eq!(
        report.steps, 2,
        "case-insensitive target selects the builder stage"
    );
}

#[test]
fn no_target_builds_final_stage_unchanged() {
    // `target = None` ⇒ final stage (byte-identical to `build`): /app.txt exists.
    let f = fix();
    std::fs::write(f.ctx_path.join("app.txt"), b"final").unwrap();
    let df = "FROM scratch AS builder\n\
              COPY app.txt /out/app.txt\n\
              FROM scratch\n\
              COPY --from=builder /out/app.txt /app.txt\n";
    let report = build_target_df(&f, "wpc-final", df, None).unwrap();
    assert_eq!(
        report.steps, 4,
        "all four steps run for the final-stage build"
    );
    let dest = hydrate(&f, "wpc-final", "final");
    assert_eq!(
        std::fs::read_to_string(dest.join("app.txt")).unwrap(),
        "final"
    );
}

#[test]
fn unknown_target_is_honest_error() {
    let f = fix();
    std::fs::write(f.ctx_path.join("a.txt"), b"x").unwrap();
    let df = "FROM scratch AS builder\nCOPY a.txt /a.txt\n";
    let msg = expect_err(build_target_df(&f, "wpc-bogus", df, Some("nope")));
    let lc = msg.to_lowercase();
    assert!(
        lc.contains("no such stage") && lc.contains("nope"),
        "honest unknown-target error naming the stage: {msg}"
    );
}

#[test]
fn target_final_stage_by_name_outputs_final() {
    // `--target` naming the LAST stage builds everything (loop runs to the end).
    let f = fix();
    std::fs::write(f.ctx_path.join("app.txt"), b"z").unwrap();
    let df = "FROM scratch AS builder\n\
              COPY app.txt /out/app.txt\n\
              FROM scratch AS final\n\
              COPY --from=builder /out/app.txt /app.txt\n";
    let report = build_target_df(&f, "wpc-lastname", df, Some("final")).unwrap();
    assert_eq!(report.steps, 4, "targeting the last stage builds all steps");
    let dest = hydrate(&f, "wpc-lastname", "lastname");
    assert_eq!(std::fs::read_to_string(dest.join("app.txt")).unwrap(), "z");
}

// ── FROM --platform: validation + memo-key fold ───────────────────────────────

#[test]
fn from_platform_mismatch_against_recorded_base_errors() {
    // A base image whose ACTUAL recorded platform is linux/amd64, built FROM with
    // --platform=linux/arm64, must fail closed (single-arch import can't select a
    // different platform). The base needs hydrate-able content + a manifest record.
    let f = fix();
    // Materialize a base ref with a tiny tree so hydrate would succeed if reached.
    let base_dir = f.store_tmp_path.join("base-src");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::write(base_dir.join("f"), b"base").unwrap();
    {
        // snapshot reads the process-global LIGHTR_HOME (codec); hold the shared READ
        // lock just for it — build_target_df below takes its own (no re-entrant read).
        let _env = crate::build::LIGHTR_HOME_ENV_LOCK
            .read()
            .unwrap_or_else(|e| e.into_inner());
        lightr_index::snapshot(&base_dir, &f.store, "amd64base").unwrap();
    }
    f.store
        .image_manifest_put(
            "amd64base",
            &ImageManifestRecord {
                manifest_bytes: b"{}".to_vec(),
                descriptors: Vec::new(),
                platform: "linux/amd64".to_string(),
            },
        )
        .unwrap();
    let df = "FROM --platform=linux/arm64 amd64base\n";
    let msg = expect_err(build_target_df(&f, "wpc-platmism", df, None));
    assert!(
        msg.contains("single-arch") && msg.contains("arm64"),
        "honest platform-mismatch error: {msg}"
    );
}

#[test]
fn from_platform_matching_recorded_base_succeeds() {
    let f = fix();
    let base_dir = f.store_tmp_path.join("base-ok");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::write(base_dir.join("f"), b"base").unwrap();
    {
        // snapshot reads the process-global LIGHTR_HOME (codec); hold the shared READ
        // lock just for it — build_target_df below takes its own (no re-entrant read).
        let _env = crate::build::LIGHTR_HOME_ENV_LOCK
            .read()
            .unwrap_or_else(|e| e.into_inner());
        lightr_index::snapshot(&base_dir, &f.store, "okbase").unwrap();
    }
    f.store
        .image_manifest_put(
            "okbase",
            &ImageManifestRecord {
                manifest_bytes: b"{}".to_vec(),
                descriptors: Vec::new(),
                platform: "linux/amd64".to_string(),
            },
        )
        .unwrap();
    let df = "FROM --platform=linux/amd64 okbase\n";
    let report = build_target_df(&f, "wpc-platok", df, None).unwrap();
    assert_eq!(report.steps, 1, "the single FROM step runs");
}

#[test]
fn two_platforms_produce_distinct_memo_keys() {
    // END-TO-END memo: the SAME scratch Dockerfile built with two different
    // --platform values must NOT cross-cache. We can't pass --platform to scratch
    // via a flag-less build, so we drive the key directly through a scratch FROM
    // step at each platform and assert the keys differ.
    use super::super::memo::{step_key, ContextKey};
    use super::super::parse::parse_dockerfile;
    let f = fix();
    let steps = parse_dockerfile("FROM scratch\n").unwrap();
    let step = &steps[0];
    let ignore = super::super::dockerignore::DockerIgnore::load(&f.ctx_path);
    let scope = VarScope::default();
    let dsh = exec_instr::default_shell();
    let ck = ContextKey {
        context_dir: &f.ctx_path,
        ignore: &ignore,
    };
    let k_amd = step_key(None, step, ck, &scope, true, &dsh, None, "linux/amd64").unwrap();
    let k_arm = step_key(None, step, ck, &scope, true, &dsh, None, "linux/arm64").unwrap();
    assert_ne!(
        k_amd.0, k_arm.0,
        "two platforms must yield distinct memo keys (no false cross-platform hit)"
    );
    // Same platform ⇒ same key (deterministic).
    let k_amd2 = step_key(None, step, ck, &scope, true, &dsh, None, "linux/amd64").unwrap();
    assert_eq!(k_amd.0, k_amd2.0, "same platform ⇒ same key");
}
