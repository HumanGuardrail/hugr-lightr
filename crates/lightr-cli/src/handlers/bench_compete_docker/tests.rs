// ──────────────────────────────────────────────────────────────────────────────
// Tests — portable, NO container spawn. (The spawn probes are exercised by the
// operator at marketing time on a real box; their command construction is
// reviewed, not unit-run, so `cargo test`/CI never launch a container.)
//
// What is unit-tested here is the PURE logic we factored out of the spawn probes:
// the timeout/poll helper against non-docker child processes (`true`/`sleep`),
// the fallible median sampler, and the unique-name generator. None of these spawn
// docker.
// ──────────────────────────────────────────────────────────────────────────────

use super::probes::{dir_size_bytes, docker_app_candidates};
use super::{median_outcome, run_op, sample_median, unique_name, Outcome, OP_TIMEOUT};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::super::bench_compare::SAMPLES;

#[test]
fn dir_size_sums_regular_files_not_symlinks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("a.bin"), vec![0u8; 1000]).expect("write a");
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    std::fs::write(root.join("sub/b.bin"), vec![0u8; 2000]).expect("write b");
    let total = dir_size_bytes(root).expect("readable root");
    assert_eq!(total, 3000, "sum of regular-file sizes under the tree");
}

#[test]
fn dir_size_missing_root_is_none() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("does-not-exist");
    assert!(
        dir_size_bytes(&missing).is_none(),
        "unreadable root must be None, never a fabricated 0"
    );
}

#[test]
fn docker_app_candidates_includes_standard_location() {
    let cands = docker_app_candidates(Path::new("/usr/local/bin/docker"));
    assert!(
        cands
            .iter()
            .any(|p| p == Path::new("/Applications/Docker.app")),
        "standard macOS install location must be a candidate"
    );
}

#[test]
fn docker_app_candidates_walks_up_to_app_bundle() {
    // A binary nested under a .app bundle → the bundle itself is a candidate.
    let cands = docker_app_candidates(Path::new(
        "/Applications/Docker.app/Contents/Resources/bin/docker",
    ));
    assert!(
        cands
            .iter()
            .any(|p| p == Path::new("/Applications/Docker.app")),
        "a *.app ancestor of the binary must be a candidate"
    );
}

// ── Timeout/poll helper (drives `true`/`sleep`, NOT docker) ────────────────

#[test]
fn run_op_returns_a_duration_on_clean_success() {
    // `true` exits 0 immediately → a real, non-negative duration, never an err.
    let d = run_op(&mut Command::new("true"), OP_TIMEOUT).expect("true exits 0");
    assert!(d >= Duration::ZERO);
}

#[test]
fn run_op_nonzero_exit_is_err_never_a_number() {
    // `false` exits 1 → Err (would become an honest SKIP), NOT a fabricated 0.
    assert!(
        run_op(&mut Command::new("false"), OP_TIMEOUT).is_err(),
        "a non-zero exit must be a failure, never a measured number"
    );
}

#[test]
fn run_op_spawn_error_is_err() {
    // A binary that cannot exist → spawn fails → Err (honest failure upstream).
    assert!(
        run_op(
            &mut Command::new("definitely-not-a-real-binary-xyz-9999"),
            OP_TIMEOUT
        )
        .is_err(),
        "a spawn error must be a failure, never a measured number"
    );
}

#[test]
fn run_op_kills_on_timeout_and_reports_failure() {
    // `sleep 30` against a tiny timeout must be killed and reported as failure
    // well before the 30s — proving the deadline bounds every spawned op.
    let start = Instant::now();
    let r = run_op(Command::new("sleep").arg("30"), Duration::from_millis(150));
    assert!(
        r.is_err(),
        "a timed-out op must be a failure, never a number"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the op must be killed near the deadline, not run to completion"
    );
}

// ── Fallible median sampler (pure; no spawn) ───────────────────────────────

#[test]
fn sample_median_picks_the_middle_of_sorted_samples() {
    // Deterministic durations 5,1,3,2,4 ms (+ a warmup we ignore). After sort:
    // 1,2,3,4,5 → median index SAMPLES/2 = 2 → 3 ms. SAMPLES is 5.
    assert_eq!(SAMPLES, 5, "this test assumes the frozen SAMPLES = 5");
    let mut seq = [
        5u64, // warmup (discarded)
        5, 1, 3, 2, 4, // the 5 timed samples
    ]
    .into_iter();
    let d = sample_median("unused", || Ok(Duration::from_millis(seq.next().unwrap())))
        .expect("all samples ok");
    assert_eq!(d, Duration::from_millis(3));
}

#[test]
fn sample_median_warmup_failure_is_skip() {
    // The FIRST call (warmup) fails → SKIP with the static reason, no number.
    let r = sample_median("warmup boom", || Err(()));
    assert_eq!(r, Err("warmup boom"));
}

#[test]
fn sample_median_any_sample_failure_is_skip() {
    // Warmup + first sample ok, then a failure → SKIP (never a partial median).
    let mut n = 0usize;
    let r = sample_median("sampling boom", || {
        n += 1;
        // call 1 = warmup ok, call 2 = sample ok, call 3 = fail.
        if n >= 3 {
            Err(())
        } else {
            Ok(Duration::from_millis(1))
        }
    });
    assert_eq!(r, Err("sampling boom"));
}

#[test]
fn median_outcome_maps_ok_to_measured_and_err_to_skip() {
    match median_outcome(Ok(Duration::from_millis(7))) {
        Outcome::Measured(ms) => assert!((ms - 7.0).abs() < 1e-9),
        Outcome::Skip(_) => panic!("ok must map to Measured"),
    }
    match median_outcome(Err("nope")) {
        Outcome::Skip(r) => assert_eq!(r, "nope"),
        Outcome::Measured(_) => panic!("err must map to Skip"),
    }
}

// ── Unique names (no timestamps; collision-free within a process) ──────────

#[test]
fn unique_name_is_namespaced_and_monotonic() {
    let a = unique_name("build");
    let b = unique_name("build");
    let pid = std::process::id();
    assert!(a.starts_with(&format!("lightr-bench-build-{pid}-")));
    assert!(b.starts_with(&format!("lightr-bench-build-{pid}-")));
    assert_ne!(a, b, "successive names must differ (monotonic counter)");
}

#[test]
fn unique_name_distinguishes_kinds() {
    let img = unique_name("mat");
    let ctx = unique_name("mat-ctx");
    assert!(img.contains("-mat-"));
    assert!(ctx.contains("-mat-ctx-"));
}
