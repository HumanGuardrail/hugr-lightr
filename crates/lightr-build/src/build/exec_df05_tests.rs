//! WP-DF-05 end-to-end tests: ENV/LABEL multi-pair + quoting + interpolation,
//! exercised through the full `build()` loop. Split out of `exec_tests.rs` to
//! keep each file under the 400-line godfile cap.
//!
//! Each test owns its tempdirs + store and never MUTATES process-global state,
//! but `build()`/`hydrate` READ the process-global `LIGHTR_HOME`, so the `run`
//! helper (and the one in-body `hydrate` call) hold the crate-wide shared read
//! lock (`build::LIGHTR_HOME_ENV_LOCK`) to exclude the setter tests
//! (exec_tests/up_tests) while they run. Readers still parallelize; each test
//! uses a nanos-unique temp work dir.
use super::*;
use tempfile::TempDir;

/// Self-contained fixture: own context dir, own store, own counter file.
struct Fix {
    _ctx: TempDir,
    store_tmp: TempDir,
    store: Store,
    counter: std::path::PathBuf,
    ctx_path: std::path::PathBuf,
}

fn fix() -> Fix {
    let _ctx = TempDir::new().unwrap();
    let store_tmp = TempDir::new().unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let counter = store_tmp.path().join("counter.txt");
    let ctx_path = _ctx.path().to_path_buf();
    Fix {
        _ctx,
        store_tmp,
        store,
        counter,
        ctx_path,
    }
}

/// Write `df_body` (`{CF}` → counter path) and run `build()`.
fn run(f: &Fix, name: &str, df_body: &str) -> BuildReport {
    let df = df_body.replace("{CF}", &f.counter.to_string_lossy());
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(&df_path, &df).unwrap();
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
    .unwrap()
}

#[test]
fn env_multi_pair_all_keys_reach_run() {
    // `ENV A=1 B=2 C=3` must put ALL THREE into the RUN environment (scope +
    // accumulated_env), proven by echoing each into the counter file.
    let f = fix();
    run(
        &f,
        "df05-multi",
        "FROM scratch\nENV A=1 B=2 C=3\nRUN /bin/sh -c 'echo ${A}-${B}-${C} >> {CF}'\n",
    );
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "1-2-3\n",
        "all multi-pair ENV keys must be set in the scope"
    );
}

#[test]
fn env_quoted_value_with_spaces_reaches_run() {
    // `ENV MSG="hello world"` keeps the space; the quoted value interpolates whole.
    let f = fix();
    run(
        &f,
        "df05-quote",
        "FROM scratch\nENV MSG=\"hello world\"\nRUN /bin/sh -c 'echo [${MSG}] >> {CF}'\n",
    );
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "[hello world]\n",
        "quoted ENV value must keep its spaces"
    );
}

#[test]
fn env_value_interpolates_prior_env() {
    // A later ENV pair references an earlier ENV var (set in a prior instruction).
    let f = fix();
    run(
        &f,
        "df05-interp",
        "FROM scratch\nENV BASE=/opt\nENV BIN=${BASE}/bin\nRUN /bin/sh -c 'echo ${BIN} >> {CF}'\n",
    );
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "/opt/bin\n",
        "ENV value must interpolate a prior ENV var"
    );
}

#[test]
fn env_legacy_single_pair_behavior_preserved() {
    // Legacy `ENV K v` (whole rest is the value) builds identically to before.
    let f = fix();
    run(
        &f,
        "df05-legacy",
        "FROM scratch\nENV GREETING hello there\nRUN /bin/sh -c 'echo [${GREETING}] >> {CF}'\n",
    );
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "[hello there]\n",
        "legacy ENV K v must keep the whole rest as the value"
    );
}

#[test]
fn env_one_pair_change_busts_cache_no_false_hit() {
    // Changing ONE pair in a multi-pair ENV changes the interpolated RUN text
    // (B's value flows into the RUN) → key differs (via WP-DF-BUILDKEY, untouched)
    // → RUN re-runs. Proven by a side effect; memo.rs is NOT edited.
    let f = fix();
    run(
        &f,
        "df05-bust",
        "FROM scratch\nENV A=1 B=2\nRUN /bin/sh -c 'echo ${A}-${B} >> {CF}'\n",
    );
    assert_eq!(std::fs::read_to_string(&f.counter).unwrap(), "1-2\n");
    // Flip B from 2 to 9 — same store, different interpolated RUN text.
    run(
        &f,
        "df05-bust",
        "FROM scratch\nENV A=1 B=9\nRUN /bin/sh -c 'echo ${A}-${B} >> {CF}'\n",
    );
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "1-2\n1-9\n",
        "changing one ENV pair must NOT reuse the cached layer (no false hit)"
    );
}

#[test]
fn label_multi_pair_recorded_in_image_meta() {
    // `LABEL a=1 b="two words"` must record BOTH labels in the image sidecar
    // (NOT the VarScope). Hydrate the result and read back `.lightr-image.json`.
    let f = fix();
    run(
        &f,
        "df05-label",
        "FROM scratch\nLABEL a=1 b=\"two words\"\nRUN /bin/sh -c 'echo x >> {CF}'\n",
    );
    let dest = f.store_tmp.path().join("hydrated");
    // hydrate READs the process-global LIGHTR_HOME; hold the shared read lock.
    let _env = crate::build::LIGHTR_HOME_ENV_LOCK
        .read()
        .unwrap_or_else(|e| e.into_inner());
    lightr_index::hydrate(&dest, &f.store, "df05-label").unwrap();
    let meta_raw = std::fs::read_to_string(dest.join(".lightr-image.json")).unwrap();
    assert!(
        meta_raw.contains("\"a\""),
        "label a must be recorded: {meta_raw}"
    );
    assert!(meta_raw.contains("\"1\""), "label a's value: {meta_raw}");
    assert!(
        meta_raw.contains("\"b\""),
        "label b must be recorded: {meta_raw}"
    );
    assert!(
        meta_raw.contains("two words"),
        "label b's quoted value: {meta_raw}"
    );
}
