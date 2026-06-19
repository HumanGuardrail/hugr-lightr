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
