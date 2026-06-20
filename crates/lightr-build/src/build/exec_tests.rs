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
    )
    .unwrap();
    assert_eq!(r1.cached_steps, 0, "cold no-var build runs every step");
    let r2 = build(
        ctx.path(),
        &df_path,
        "buildkey-novar",
        lightr_engine::EngineKind::Native,
        &store,
    )
    .unwrap();
    assert_eq!(
        r2.cached_steps, r2.steps,
        "no-var rebuild is fully cached (stable v2 key)"
    );

    std::env::remove_var("LIGHTR_HOME");
}
