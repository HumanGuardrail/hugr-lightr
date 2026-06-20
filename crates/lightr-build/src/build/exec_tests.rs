use super::*;
use std::sync::Mutex;
use tempfile::TempDir;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

#[test]
fn step_reads_clock_or_net_heuristic() {
    let date_cmd = vec!["/bin/sh".to_string(), "-c".to_string(), "date".to_string()];
    assert!(step_reads_clock_or_net(&date_cmd));
    let echo_cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo hi".to_string(),
    ];
    assert!(!step_reads_clock_or_net(&echo_cmd));
    let curl_cmd = vec!["curl".to_string(), "https://example.com".to_string()];
    assert!(step_reads_clock_or_net(&curl_cmd));
}

#[test]
fn build_memoization_scratch_copy_run() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = TempDir::new().unwrap();
    let store_tmp = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", store_tmp.path());
    let counter_file = store_tmp.path().join("counter.txt");
    std::fs::write(&counter_file, "0").unwrap();
    let src_file = ctx.path().join("hello.txt");
    std::fs::write(&src_file, b"hello").unwrap();
    let df_path = ctx.path().join("Dockerfile");
    let counter_path_str = counter_file.to_string_lossy();
    let df_content = format!(
        "FROM scratch\nCOPY hello.txt /hello.txt\nRUN /bin/sh -c 'v=$(cat {counter_path_str}); echo $((v+1)) > {counter_path_str}'\n"
    );
    std::fs::write(&df_path, &df_content).unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let report1 = build(
        ctx.path(),
        &df_path,
        "test-build",
        lightr_engine::EngineKind::Native,
        &store,
        &[],
    )
    .unwrap();
    assert_eq!(report1.steps, 3);
    assert_eq!(report1.cached_steps, 0);
    let c1: u32 = std::fs::read_to_string(&counter_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(c1, 1, "RUN should increment to 1");
    let report2 = build(
        ctx.path(),
        &df_path,
        "test-build",
        lightr_engine::EngineKind::Native,
        &store,
        &[],
    )
    .unwrap();
    assert_eq!(report2.cached_steps, 3, "all steps must be cache hits");
    let c2: u32 = std::fs::read_to_string(&counter_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(c2, 1, "counter must NOT increment on cache hit");
    std::fs::write(&src_file, b"changed").unwrap();
    let report3 = build(
        ctx.path(),
        &df_path,
        "test-build",
        lightr_engine::EngineKind::Native,
        &store,
        &[],
    )
    .unwrap();
    assert!(
        report3.cached_steps < 3,
        "COPY+RUN must not be cached after file change"
    );
    let c3: u32 = std::fs::read_to_string(&counter_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(c3, 2, "RUN must re-run after COPY file changed");
    std::env::remove_var("LIGHTR_HOME");
}

#[test]
fn build_hydrate_final_tree() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = TempDir::new().unwrap();
    let store_tmp = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", store_tmp.path());
    std::fs::write(ctx.path().join("src.txt"), b"content").unwrap();
    let df_path = ctx.path().join("Dockerfile");
    std::fs::write(
        &df_path,
        "FROM scratch\nCOPY src.txt /src.txt\nRUN echo built\n",
    )
    .unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let report = build(
        ctx.path(),
        &df_path,
        "test-hydrate",
        lightr_engine::EngineKind::Native,
        &store,
        &[],
    )
    .unwrap();
    assert_eq!(report.steps, 3);
    assert_eq!(report.cached_steps, 0);
    let dest = store_tmp.path().join("hydrated");
    lightr_index::hydrate(&dest, &store, "test-hydrate").unwrap();
    assert!(
        dest.join("src.txt").exists(),
        "/src.txt must be in hydrated tree"
    );
    let src_content = std::fs::read_to_string(dest.join("src.txt")).unwrap();
    assert_eq!(src_content, "content");
    std::env::remove_var("LIGHTR_HOME");
}

// ---- WP-DF-BUILDKEY: end-to-end MEMO-CORRECTNESS over the full build() ----

/// Build a counter-incrementing Dockerfile whose RUN echoes `${X}` set by ENV.
/// The RUN appends a line to `counter_file` on every actual execution, so the
/// file's line-count proves how many times RUN really ran (vs. was a memo hit).
fn run_build_with_x(
    ctx: &std::path::Path,
    store: &Store,
    x_value: &str,
    counter_file: &std::path::Path,
) -> BuildReport {
    let cf = counter_file.to_string_lossy();
    let df = format!("FROM scratch\nENV X={x_value}\nRUN /bin/sh -c 'echo ${{X}} >> {cf}'\n");
    let df_path = ctx.join("Dockerfile");
    std::fs::write(&df_path, &df).unwrap();
    build(
        ctx,
        &df_path,
        "buildkey-memo",
        lightr_engine::EngineKind::Native,
        store,
        &[],
    )
    .unwrap()
}

#[test]
fn interpolated_env_value_change_does_not_false_hit() {
    // CORE acceptance: the SAME raw `RUN echo ${X}` keyed under X=A vs X=B must
    // NOT reuse A's cached layer — interpolation makes the step text (and thus
    // the key) differ, so RUN re-runs for B. Proven via a real side effect.
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = TempDir::new().unwrap();
    let store_tmp = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", store_tmp.path());
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let counter = store_tmp.path().join("counter.txt");

    // Build with X=alpha → RUN executes once.
    run_build_with_x(ctx.path(), &store, "alpha", &counter);
    let after_a = std::fs::read_to_string(&counter).unwrap();
    assert_eq!(after_a, "alpha\n", "RUN must run once for X=alpha");

    // Build with X=beta against the SAME store → DIFFERENT interpolated text ⇒
    // different RUN key ⇒ NO false memo hit ⇒ RUN executes again.
    run_build_with_x(ctx.path(), &store, "beta", &counter);
    let after_b = std::fs::read_to_string(&counter).unwrap();
    assert_eq!(
        after_b, "alpha\nbeta\n",
        "X=beta must NOT reuse X=alpha's cached layer (no false memo hit)"
    );

    std::env::remove_var("LIGHTR_HOME");
}

#[test]
fn identical_interpolated_build_is_a_memo_hit() {
    // Identical inputs (same ENV value) ⇒ identical key ⇒ memo HIT: the RUN
    // side effect does NOT repeat on the second build.
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = TempDir::new().unwrap();
    let store_tmp = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", store_tmp.path());
    let store = Store::open(store_tmp.path().join("store")).unwrap();
    let counter = store_tmp.path().join("counter.txt");

    let r1 = run_build_with_x(ctx.path(), &store, "same", &counter);
    assert_eq!(r1.cached_steps, 0, "cold build: no cache hits");
    let r2 = run_build_with_x(ctx.path(), &store, "same", &counter);
    assert_eq!(
        r2.cached_steps, r2.steps,
        "identical inputs ⇒ every step is a memo hit"
    );
    let content = std::fs::read_to_string(&counter).unwrap();
    assert_eq!(
        content, "same\n",
        "RUN must NOT re-run on an identical-input rebuild (memo hit)"
    );

    std::env::remove_var("LIGHTR_HOME");
}

#[test]
fn no_var_dockerfile_builds_and_is_stable() {
    // Behavior-preserving: a Dockerfile with NO ${VAR} builds correctly and is
    // fully cached on an identical rebuild (stable key under v2).
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = TempDir::new().unwrap();
    let store_tmp = TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", store_tmp.path());
    std::fs::write(ctx.path().join("f.txt"), b"data").unwrap();
    let df_path = ctx.path().join("Dockerfile");
    std::fs::write(
        &df_path,
        "FROM scratch\nCOPY f.txt /f.txt\nRUN echo plain\n",
    )
    .unwrap();
    let store = Store::open(store_tmp.path().join("store")).unwrap();

    let r1 = build(
        ctx.path(),
        &df_path,
        "buildkey-novar",
        lightr_engine::EngineKind::Native,
        &store,
        &[],
    )
    .unwrap();
    assert_eq!(r1.cached_steps, 0, "cold no-var build runs every step");
    let r2 = build(
        ctx.path(),
        &df_path,
        "buildkey-novar",
        lightr_engine::EngineKind::Native,
        &store,
        &[],
    )
    .unwrap();
    assert_eq!(
        r2.cached_steps, r2.steps,
        "no-var rebuild is fully cached (stable v2 key)"
    );

    std::env::remove_var("LIGHTR_HOME");
}

// ---- WP-DF-08: ARG instruction + --build-arg end-to-end MEMO-correctness ----
// These tests pass the store + counter EXPLICITLY (build() does not read
// LIGHTR_HOME and uses a nanos-unique temp work dir), so they need NO env mutation
// and NO shared mutex — parallel-safe by construction (own tempdirs per test).

struct ArgFix {
    _ctx: TempDir,
    _store_tmp: TempDir,
    store: Store,
    counter: std::path::PathBuf,
    ctx_path: std::path::PathBuf,
}

fn arg_fix() -> ArgFix {
    let _ctx = TempDir::new().unwrap();
    let _store_tmp = TempDir::new().unwrap();
    let store = Store::open(_store_tmp.path().join("store")).unwrap();
    let counter = _store_tmp.path().join("counter.txt");
    let ctx_path = _ctx.path().to_path_buf();
    ArgFix {
        _ctx,
        _store_tmp,
        store,
        counter,
        ctx_path,
    }
}

/// Write `df_body` (with `{CF}` → counter path) and run `build()` with `build_args`.
fn run_build_with_arg(f: &ArgFix, df_body: &str, build_args: &[(String, String)]) -> BuildReport {
    let df = df_body.replace("{CF}", &f.counter.to_string_lossy());
    let df_path = f.ctx_path.join("Dockerfile");
    std::fs::write(&df_path, &df).unwrap();
    build(
        &f.ctx_path,
        &df_path,
        "arg-memo",
        lightr_engine::EngineKind::Native,
        &f.store,
        build_args,
    )
    .unwrap()
}

fn arg(k: &str, v: &str) -> Vec<(String, String)> {
    vec![(k.to_string(), v.to_string())]
}

#[test]
fn arg_default_is_used_in_run() {
    // `ARG GREET=hi` + `RUN echo ${GREET}` with NO --build-arg → the default is
    // interpolated into the RUN, proven by the side effect.
    let f = arg_fix();
    let df = "FROM scratch\nARG GREET=hi\nRUN /bin/sh -c 'echo ${GREET} >> {CF}'\n";
    run_build_with_arg(&f, df, &[]);
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "hi\n",
        "ARG default must be interpolated into RUN"
    );
}

#[test]
fn build_arg_override_busts_cache_no_false_hit() {
    // CORE acceptance: a different --build-arg value used in a RUN must NOT reuse
    // the prior cached layer — the override changes the interpolated RUN text →
    // key differs (via WP-DF-BUILDKEY) → RUN re-runs. The build key is NOT touched
    // by WP-DF-08; this verifies the automatic correctness via a side effect.
    let f = arg_fix();
    let df = "FROM scratch\nARG GREET=hi\nRUN /bin/sh -c 'echo ${GREET} >> {CF}'\n";
    run_build_with_arg(&f, df, &arg("GREET", "alpha"));
    assert_eq!(std::fs::read_to_string(&f.counter).unwrap(), "alpha\n");
    run_build_with_arg(&f, df, &arg("GREET", "beta"));
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "alpha\nbeta\n",
        "different --build-arg must NOT reuse the cached layer (no false hit)"
    );
}

#[test]
fn unused_arg_does_not_bust_cache() {
    // An ARG NOT referenced by any instruction changes no instruction text → no
    // key change → rebuild is a full memo HIT even when the --build-arg value
    // differs (matches Docker). The RUN side effect must NOT repeat.
    let f = arg_fix();
    let df = "FROM scratch\nARG UNUSED=x\nRUN /bin/sh -c 'echo run >> {CF}'\n";
    let r1 = run_build_with_arg(&f, df, &arg("UNUSED", "first"));
    assert_eq!(r1.cached_steps, 0, "cold build runs every step");
    assert_eq!(std::fs::read_to_string(&f.counter).unwrap(), "run\n");
    let r2 = run_build_with_arg(&f, df, &arg("UNUSED", "second"));
    assert_eq!(
        r2.cached_steps, r2.steps,
        "unused ARG change must be a full memo hit"
    );
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "run\n",
        "RUN must NOT re-run when only an UNUSED ARG changed"
    );
}

#[test]
fn global_arg_before_from_is_usable_in_from() {
    // A global ARG (before FROM) interpolates into the FROM line. `scratch` is the
    // only base the native builder hydrates without a registry, so a global ARG
    // whose --build-arg resolves `FROM ${BASE}` to `scratch` proves it reaches FROM.
    let f = arg_fix();
    let df = "ARG BASE=scratch\nFROM ${BASE}\nRUN /bin/sh -c 'echo built >> {CF}'\n";
    let report = run_build_with_arg(&f, df, &arg("BASE", "scratch"));
    assert_eq!(report.cached_steps, 0, "cold build");
    assert_eq!(
        std::fs::read_to_string(&f.counter).unwrap(),
        "built\n",
        "global ARG must resolve the FROM ref and the build must run"
    );
}
