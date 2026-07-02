//! WP-DF-03 multi-stage end-to-end tests, exercised through the full `build()`
//! loop + a hydrate of the final tree. Split out of `exec_tests.rs` for the
//! godfile cap.
//!
//! Each test owns its tempdirs + store and never MUTATES process-global state,
//! but `build()`/`hydrate` READ the process-global `LIGHTR_HOME`, so the
//! `build_df`/`hydrate` helpers hold the crate-wide shared read lock
//! (`build::LIGHTR_HOME_ENV_LOCK`) to exclude the setter tests
//! (exec_tests/up_tests) while they run. Readers still parallelize among
//! themselves; each test uses a nanos-unique temp work dir.
//!
//! Stages produce their artifacts via COPY (no RUN), so the tests need no shell
//! and run on any box: a `FROM scratch` stage + `COPY ctx-file /dst` is a
//! deterministic stage output; a later stage's `COPY --from=<stage>` pulls it.
use super::*;
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

fn build_df(f: &Fix, name: &str, df_body: &str) -> Result<BuildReport> {
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(&df_path, df_body).unwrap();
    // build() READs the process-global LIGHTR_HOME; hold the crate-wide shared
    // read lock so a concurrent setter cannot flip the home mid-build.
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
    build(
        &f.ctx_path,
        &df_path,
        name,
        lightr_engine::EngineKind::Native,
        &f.store,
        &[],
    )
}

fn build_err(f: &Fix, name: &str, df_body: &str) -> String {
    match build_df(f, name, df_body) {
        Ok(_) => panic!("expected build to fail, but it succeeded"),
        Err(e) => format!("{e}"),
    }
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

#[test]
fn two_stage_copy_from_named_pulls_builder_artifact() {
    // The canonical multi-stage shape: a `builder` stage produces an artifact,
    // the final stage COPYs it in by NAME. The build OUTPUT is the LAST stage —
    // so the hydrated tree has the copied-in artifact but NOT the builder's
    // private context file under its original path.
    let f = fix();
    std::fs::write(f.ctx_path.join("app.txt"), b"built-artifact").unwrap();
    let df = "FROM scratch AS builder\n\
              COPY app.txt /out/app.txt\n\
              FROM scratch\n\
              COPY --from=builder /out/app.txt /app.txt\n";
    build_df(&f, "df03-named", df).unwrap();
    let dest = hydrate(&f, "df03-named", "named");
    assert_eq!(
        std::fs::read_to_string(dest.join("app.txt")).unwrap(),
        "built-artifact",
        "COPY --from=builder must pull the builder stage's artifact"
    );
    assert!(
        !dest.join("out/app.txt").exists(),
        "the final stage's tree is the OUTPUT — the builder's /out path is not in it"
    );
}

#[test]
fn copy_from_by_index_pulls_prior_stage() {
    // `COPY --from=0` references the FIRST stage by its 0-based build index.
    let f = fix();
    std::fs::write(f.ctx_path.join("lib.txt"), b"index-artifact").unwrap();
    let df = "FROM scratch\n\
              COPY lib.txt /lib.txt\n\
              FROM scratch\n\
              COPY --from=0 /lib.txt /pulled.txt\n";
    build_df(&f, "df03-index", df).unwrap();
    let dest = hydrate(&f, "df03-index", "index");
    assert_eq!(
        std::fs::read_to_string(dest.join("pulled.txt")).unwrap(),
        "index-artifact",
        "COPY --from=0 must pull stage index 0's output"
    );
}

#[test]
fn copy_from_unknown_stage_is_honest_error() {
    // A ref to a stage that does not exist is a fail-closed error, never silent.
    let f = fix();
    std::fs::write(f.ctx_path.join("x.txt"), b"x").unwrap();
    let msg = build_err(
        &f,
        "df03-unknown",
        "FROM scratch\nCOPY --from=nope /x.txt /x.txt\n",
    );
    assert!(
        msg.to_lowercase().contains("unknown stage"),
        "unknown stage ref must be an honest error, got: {msg}"
    );
}

#[test]
fn copy_from_forward_ref_is_honest_error() {
    // A FORWARD ref (a stage defined LATER) must NOT resolve — only prior stages
    // are in the table when the COPY runs. Here stage 0 references `later` (the
    // stage at index 1), which has not been built yet ⇒ honest error.
    let f = fix();
    std::fs::write(f.ctx_path.join("x.txt"), b"x").unwrap();
    let df = "FROM scratch\n\
              COPY --from=later /x.txt /x.txt\n\
              FROM scratch AS later\n\
              COPY x.txt /x.txt\n";
    let msg = build_err(&f, "df03-forward", df);
    assert!(
        msg.to_lowercase().contains("unknown stage"),
        "a forward stage ref must not resolve (honest error), got: {msg}"
    );
}

#[test]
fn copy_from_self_ref_by_index_is_honest_error() {
    // A SELF ref by index (stage 1 copying `--from=1`) must not resolve: stage 1
    // is in progress, not yet recorded, so index 1 is out of range ⇒ honest error.
    let f = fix();
    std::fs::write(f.ctx_path.join("x.txt"), b"x").unwrap();
    let df = "FROM scratch\n\
              COPY x.txt /x.txt\n\
              FROM scratch\n\
              COPY --from=1 /x.txt /y.txt\n";
    let msg = build_err(&f, "df03-self", df);
    assert!(
        msg.to_lowercase().contains("out of range") || msg.to_lowercase().contains("no such"),
        "a self/forward index ref must be an honest out-of-range error, got: {msg}"
    );
}

#[test]
fn copy_from_external_image_is_out_of_scope_error() {
    // `COPY --from=<external image>` is OUT OF SCOPE for this WP: an image ref
    // (not a stage name/index) does not resolve and surfaces an honest error
    // mentioning it is out of scope / unknown stage — never a silent fetch.
    let f = fix();
    std::fs::write(f.ctx_path.join("x.txt"), b"x").unwrap();
    let msg = build_err(
        &f,
        "df03-external",
        "FROM scratch\nCOPY --from=alpine:3.19 /etc/hostname /h\n",
    );
    let lc = msg.to_lowercase();
    assert!(
        lc.contains("out of scope") || lc.contains("unknown stage"),
        "an external-image --from must be an honest out-of-scope error, got: {msg}"
    );
}

#[test]
fn changing_builder_stage_busts_dependent_copy_no_false_hit() {
    // END-TO-END no-false-hit: rebuild the SAME multi-stage Dockerfile after the
    // builder's INPUT changed (app.txt content). The dependent `COPY --from`
    // step folds the upstream stage's output digest into its key, so it MUST NOT
    // reuse the first build's copied layer — the final tree reflects the NEW
    // artifact. A false hit would keep the stale content.
    let f = fix();
    let df = "FROM scratch AS builder\n\
              COPY app.txt /out/app.txt\n\
              FROM scratch\n\
              COPY --from=builder /out/app.txt /app.txt\n";

    std::fs::write(f.ctx_path.join("app.txt"), b"v1").unwrap();
    build_df(&f, "df03-bust", df).unwrap();
    let d1 = hydrate(&f, "df03-bust", "bust1");
    assert_eq!(std::fs::read_to_string(d1.join("app.txt")).unwrap(), "v1");

    // Change the builder's input; rebuild against the SAME store.
    std::fs::write(f.ctx_path.join("app.txt"), b"v2-changed").unwrap();
    let r2 = build_df(&f, "df03-bust", df).unwrap();
    assert!(
        r2.cached_steps < r2.steps,
        "a changed builder must NOT be a full cache hit (no false hit)"
    );
    let d2 = hydrate(&f, "df03-bust", "bust2");
    assert_eq!(
        std::fs::read_to_string(d2.join("app.txt")).unwrap(),
        "v2-changed",
        "the dependent COPY --from must reflect the new builder artifact"
    );
}

#[test]
fn identical_multi_stage_rebuild_is_full_memo_hit() {
    // Determinism: an unchanged multi-stage build re-run against the same store
    // is a full memo hit (the upstream-digest fold is deterministic, not a
    // blanket buster).
    let f = fix();
    std::fs::write(f.ctx_path.join("app.txt"), b"stable").unwrap();
    let df = "FROM scratch AS builder\n\
              COPY app.txt /out/app.txt\n\
              FROM scratch\n\
              COPY --from=builder /out/app.txt /app.txt\n";
    let r1 = build_df(&f, "df03-hit", df).unwrap();
    assert_eq!(r1.cached_steps, 0, "cold build");
    let r2 = build_df(&f, "df03-hit", df).unwrap();
    assert_eq!(
        r2.cached_steps, r2.steps,
        "identical multi-stage rebuild ⇒ every step is a memo hit"
    );
}

#[test]
fn single_stage_build_is_behavior_and_key_preserved() {
    // BEHAVIOR-PRESERVED: a single-FROM Dockerfile builds exactly as before —
    // the file lands, and a re-run against the same store is a FULL memo hit
    // (the key is byte-identical: from_stage_digest is None, so step_key folds
    // nothing new). Same root across runs proves the output is stable too.
    let f = fix();
    std::fs::write(f.ctx_path.join("src.txt"), b"content").unwrap();
    let df = "FROM scratch\nCOPY src.txt /src.txt\n";
    let r1 = build_df(&f, "df03-single", df).unwrap();
    assert_eq!(r1.cached_steps, 0, "cold build");
    let root1 = r1.root;
    let dest = hydrate(&f, "df03-single", "single");
    assert_eq!(
        std::fs::read_to_string(dest.join("src.txt")).unwrap(),
        "content"
    );
    let r2 = build_df(&f, "df03-single", df).unwrap();
    assert_eq!(
        r2.cached_steps, r2.steps,
        "single-stage rebuild ⇒ full memo hit (key byte-identical)"
    );
    assert_eq!(
        root1.0, r2.root.0,
        "single-stage root must be stable across runs"
    );
}
