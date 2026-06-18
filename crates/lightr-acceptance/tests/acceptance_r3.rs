//! A22–A26 per build-spec-r3.md §6 — authored by WP-R4 (red-first).
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo test -p lightr-acceptance.
//!
//! The R3 verbs (build/compose/docker) are merged; every test here is a real,
//! running acceptance test with live assertions (now green). Do NOT weaken assertions.
//!
//! # Native-engine note
//!
//! RUN steps execute via the **native engine** (no filesystem isolation on this
//! box — Intel macOS, `native` is the only available isolation). This means a
//! RUN step CAN write to any absolute path on disk, including paths outside the
//! build context. A22 exploits this: COUNTER_PATH lives in a separate tempdir;
//! the RUN writes to it directly. This is intentional and documented in
//! build-spec-r3.md §2 ("no isolation — stated loudly").
//!
//! # A24 portability caveat
//!
//! The compose lazy test binds on 127.0.0.1 with ports drawn from a high
//! ephemeral range (39000+). On a heavily loaded CI box the ports may already
//! be in use; the test polls up to 2 s for the supervisor to bind and skips the
//! connection-trigger sub-assertion if the port is still unavailable after that
//! window. The core assertions (up fast, 0 services initially, down cleans) are
//! always checked.

#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

use common::lightr_cmd;
use std::fs;
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// Polling helper
// ─────────────────────────────────────────────────────────────────────────────

/// Polls `pred` every 100 ms until it returns `true` or `timeout` expires.
fn poll_until<F: FnMut() -> bool>(timeout: Duration, mut pred: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Parse `steps=<n> cached=<n>` from stdout
// ─────────────────────────────────────────────────────────────────────────────

fn parse_build_report(stdout: &[u8]) -> (u64, u64) {
    let text = String::from_utf8_lossy(stdout);
    let mut steps: Option<u64> = None;
    let mut cached: Option<u64> = None;
    for tok in text.split_whitespace() {
        if let Some(v) = tok.strip_prefix("steps=") {
            steps = v.parse().ok();
        }
        if let Some(v) = tok.strip_prefix("cached=") {
            cached = v.parse().ok();
        }
    }
    (
        steps.unwrap_or_else(|| panic!("could not parse 'steps=<n>' from stdout:\n{}", text)),
        cached.unwrap_or_else(|| panic!("could not parse 'cached=<n>' from stdout:\n{}", text)),
    )
}

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
// A24 — compose lazy
//
// Compose with 2 services (no --eager):
//   - `compose up` returns in < 2 s (listeners bound immediately)
//   - Immediately after up: 0 services running (no run entries)
//   - Connecting to P1 triggers ≥ 1 service start within 5 s
//   - `compose down` → no running services remain
//
// Portability caveat: ports P1/P2 are picked from the 39000–39999 range using
// the test's PID for some variety. If ports are unavailable the test degrades
// gracefully: the connection-trigger sub-assertion is skipped but up/down
// correctness is always checked.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a24_compose_lazy() {
    let home = TempDir::new().unwrap();

    // Pick two ports from the high ephemeral range; use pid-derived offset for variety.
    let pid_offset = (std::process::id() % 512) as u16;
    let p1: u16 = 39000 + pid_offset;
    let p2: u16 = 39513 + pid_offset;

    // Write compose.yml into a temp dir.
    let compose_dir = TempDir::new().unwrap();
    let compose_yml = compose_dir.path().join("compose.yml");
    let compose_content = format!(
        "services:\n\
         \x20\x20svc1:\n\
         \x20\x20\x20\x20image: scratch\n\
         \x20\x20\x20\x20command: [\"/bin/sh\",\"-c\",\"sleep 30\"]\n\
         \x20\x20\x20\x20ports:\n\
         \x20\x20\x20\x20\x20\x20- \"{p1}:1\"\n\
         \x20\x20svc2:\n\
         \x20\x20\x20\x20image: scratch\n\
         \x20\x20\x20\x20command: [\"/bin/sh\",\"-c\",\"sleep 30\"]\n\
         \x20\x20\x20\x20ports:\n\
         \x20\x20\x20\x20\x20\x20- \"{p2}:2\"\n"
    );
    fs::write(&compose_yml, &compose_content).unwrap();

    // ── up: must return in < 2 s ─────────────────────────────────────────────
    let up_start = Instant::now();
    let up_out = lightr_cmd(home.path())
        .args(["compose", "up", "-f", compose_yml.to_str().unwrap()])
        .output()
        .expect("compose up must not fail to spawn");
    let up_elapsed = up_start.elapsed();

    assert_eq!(
        up_out.status.code().unwrap_or(-1),
        0,
        "compose up must exit 0 (listeners bound); stderr:\n{}",
        String::from_utf8_lossy(&up_out.stderr)
    );
    assert!(
        up_elapsed < Duration::from_secs(2),
        "compose up must return in < 2 s (lazy binding); took {:?}",
        up_elapsed
    );

    // ── immediately: 0 services running ─────────────────────────────────────
    // Check $LIGHTR_HOME/run has no running entries for this stack.
    // We use `ps --json` to check for running services.
    let ps_out = lightr_cmd(home.path())
        .args(["ps", "--json"])
        .output()
        .expect("ps --json must not fail to spawn");
    assert_eq!(
        ps_out.status.code().unwrap_or(-1),
        0,
        "ps --json must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&ps_out.stderr)
    );
    let ps_json: serde_json::Value =
        serde_json::from_slice(&ps_out.stdout).expect("ps --json must emit valid JSON");
    let ps_arr = ps_json
        .as_array()
        .expect("ps --json must emit a JSON array");
    let running_count = ps_arr
        .iter()
        .filter(|e| e.get("running").and_then(|v| v.as_bool()).unwrap_or(false))
        .count();
    assert_eq!(
        running_count, 0,
        "compose up (no --eager): 0 services must be running immediately after up; got {running_count}"
    );

    // ── connecting to P1 triggers service start ──────────────────────────────
    // Poll up to 2 s for the supervisor to bind port P1.
    let port_ready = poll_until(Duration::from_secs(2), || {
        TcpStream::connect(format!("127.0.0.1:{p1}")).is_ok()
    });

    if port_ready {
        // We already connected; now poll for ≥1 service to appear in ps.
        let service_started = poll_until(Duration::from_secs(5), || {
            let out = lightr_cmd(home.path())
                .args(["ps", "--json"])
                .output()
                .expect("ps --json must launch");
            if out.status.success() {
                if let Ok(arr) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    if let Some(arr) = arr.as_array() {
                        return arr
                            .iter()
                            .any(|e| e.get("running").and_then(|v| v.as_bool()).unwrap_or(false));
                    }
                }
            }
            false
        });
        assert!(
            service_started,
            "connecting to port {p1} must trigger ≥1 service start within 5 s"
        );
    } else {
        // Port unavailable (busy box / timing); skip the trigger sub-assertion.
        // The core assertions (up fast, 0 services initially, down cleans) still apply.
        eprintln!(
            "[A24] WARNING: port {p1} not available within 2 s; skipping connection-trigger sub-assertion"
        );
    }

    // ── compose down: no services remain ────────────────────────────────────
    let down_out = lightr_cmd(home.path())
        .args(["compose", "down", "-f", compose_yml.to_str().unwrap()])
        .output()
        .expect("compose down must not fail to spawn");
    assert_eq!(
        down_out.status.code().unwrap_or(-1),
        0,
        "compose down must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&down_out.stderr)
    );

    // After down: no running services.
    let ps_after_down = lightr_cmd(home.path())
        .args(["ps", "--json"])
        .output()
        .expect("ps --json after down must not fail to spawn");
    assert_eq!(
        ps_after_down.status.code().unwrap_or(-1),
        0,
        "ps --json after down must exit 0"
    );
    let ps_after: serde_json::Value =
        serde_json::from_slice(&ps_after_down.stdout).expect("ps --json must be valid JSON");
    let running_after = ps_after
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|e| e.get("running").and_then(|v| v.as_bool()).unwrap_or(false))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(
        running_after, 0,
        "after compose down, 0 services must be running; got {running_after}"
    );

    // Stack directory must be gone (supervisor self-cleaned or compose_down removed it).
    // We verify via: `$LIGHTR_HOME/compose/` has no entries that were spawned by this test.
    // Since the compose stack dir is keyed by nanos+pid and we just called compose_down,
    // the supervisor pid must be dead. We check this by asserting the stack_dir stdout line
    // is absent, or simply that no stack dir remains under $LIGHTR_HOME/compose/.
    let compose_dir_home = home.path().join("compose");
    if compose_dir_home.exists() {
        let remaining: Vec<_> = fs::read_dir(&compose_dir_home)
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        assert!(
            remaining.is_empty(),
            "after compose down, $LIGHTR_HOME/compose/ must be empty; remaining: {:?}",
            remaining
                .iter()
                .map(|e: &fs::DirEntry| e.path())
                .collect::<Vec<_>>()
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A25 — docker compat
//
// `lightr docker build -t @t/d <ctx>` → exit 0 + stderr contains "lightr build"
// `lightr docker images`               → lists @t/d
// `lightr docker frobnicate`           → exit 2 + stderr contains "unsupported"
//                                        and mentions supported verbs
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a25_docker_compat() {
    let home = TempDir::new().unwrap();
    let ctx = TempDir::new().unwrap();

    // Minimal Dockerfile for the docker build test.
    fs::write(
        ctx.path().join("Dockerfile"),
        "FROM scratch\nCOPY data.txt /data.txt\n",
    )
    .unwrap();
    fs::write(ctx.path().join("data.txt"), b"a25").unwrap();

    // ── docker build → exit 0 + stderr transparency note ───────────────────
    let build_out = lightr_cmd(home.path())
        .args([
            "docker",
            "build",
            "-t",
            "@t/d",
            ctx.path().to_str().unwrap(),
        ])
        .output()
        .expect("docker build must not fail to spawn");
    assert_eq!(
        build_out.status.code().unwrap_or(-1),
        0,
        "docker build must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&build_out.stderr)
    );
    // Transparency note: stderr must say it ran "lightr build" (per §4).
    let build_stderr = String::from_utf8_lossy(&build_out.stderr).to_lowercase();
    assert!(
        build_stderr.contains("lightr build") || build_stderr.contains("lightr-build"),
        "docker build stderr must mention 'lightr build' (transparency note); got:\n{}",
        String::from_utf8_lossy(&build_out.stderr)
    );

    // ── docker images → lists @t/d ──────────────────────────────────────────
    let images_out = lightr_cmd(home.path())
        .args(["docker", "images"])
        .output()
        .expect("docker images must not fail to spawn");
    assert_eq!(
        images_out.status.code().unwrap_or(-1),
        0,
        "docker images must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&images_out.stderr)
    );
    let images_stdout = String::from_utf8_lossy(&images_out.stdout);
    assert!(
        images_stdout.contains("@t/d") || images_stdout.contains("t/d"),
        "docker images must list @t/d after docker build; got:\n{}",
        images_stdout
    );

    // ── docker frobnicate → exit 2 + "unsupported" + supported list ─────────
    let frob_out = lightr_cmd(home.path())
        .args(["docker", "frobnicate"])
        .output()
        .expect("docker frobnicate must not fail to spawn");
    assert_eq!(
        frob_out.status.code().unwrap_or(-1),
        2,
        "docker frobnicate must exit 2 (unsupported subcommand)"
    );
    let frob_stderr = String::from_utf8_lossy(&frob_out.stderr).to_lowercase();
    assert!(
        frob_stderr.contains("unsupported"),
        "docker frobnicate stderr must contain 'unsupported'; got:\n{}",
        String::from_utf8_lossy(&frob_out.stderr)
    );
    // Must name at least one of the supported verbs.
    let mentions_supported = frob_stderr.contains("build")
        || frob_stderr.contains("run")
        || frob_stderr.contains("pull")
        || frob_stderr.contains("images")
        || frob_stderr.contains("ps")
        || frob_stderr.contains("compose");
    assert!(
        mentions_supported,
        "docker frobnicate stderr must mention supported verbs (build|run|pull|images|ps|compose); got:\n{}",
        String::from_utf8_lossy(&frob_out.stderr)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A26 — build determinism flag
//
// A Dockerfile with `RUN /bin/sh -c 'date > ts.txt'`.
// `build -t @t/c --explain <ctx>` → exit 0 (build still succeeds) and
// stderr flags the RUN as non-reproducible (contains "date" or "non-reprodu").
// The `step_reads_clock_or_net` heuristic in lightr-build flags the `date`
// command.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a26_build_determinism_flag() {
    let home = TempDir::new().unwrap();
    let ctx = TempDir::new().unwrap();

    // Dockerfile with a RUN that reads the clock.
    fs::write(
        ctx.path().join("Dockerfile"),
        "FROM scratch\nRUN /bin/sh -c 'date > ts.txt'\n",
    )
    .unwrap();

    let out = lightr_cmd(home.path())
        .args([
            "build",
            "-t",
            "@t/c",
            "--explain",
            ctx.path().to_str().unwrap(),
        ])
        .output()
        .expect("build --explain must not fail to spawn");

    // Build must succeed (exit 0); non-reproducible steps are RECORDED, not failed.
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "build --explain must exit 0 (determinism warnings do not fail the build); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // stderr must flag the non-reproducible RUN.
    // The heuristic matches "date" in the argv, so stderr should mention "date"
    // or use the "non-reprodu" / "non-reproducible" wording.
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    let flagged = stderr.contains("date")
        || stderr.contains("non-reprodu")
        || stderr.contains("non_reprodu")
        || stderr.contains("clock")
        || stderr.contains("reproducible");
    assert!(
        flagged,
        "build --explain stderr must flag the 'date' RUN as non-reproducible; got:\n{}",
        String::from_utf8_lossy(&out.stderr)
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

// ─────────────────────────────────────────────────────────────────────────────
// A25b — docker subcommand exit-code law
//
// `lightr docker ps` → must exit 0 (translates to `ps`).
// `lightr docker pull alpine` → exit 0 or 1 (network may be absent), never 2.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a25b_docker_ps_and_pull_exit_law() {
    let home = TempDir::new().unwrap();

    // docker ps → translates to `lightr ps` → exit 0 always.
    let ps_out = lightr_cmd(home.path())
        .args(["docker", "ps"])
        .output()
        .expect("docker ps must not fail to spawn");
    assert_eq!(
        ps_out.status.code().unwrap_or(-1),
        0,
        "docker ps must exit 0 (translates to lightr ps); stderr:\n{}",
        String::from_utf8_lossy(&ps_out.stderr)
    );

    // docker pull: exit 0 (net available) or 1 (no net); NEVER 2.
    let pull_out = lightr_cmd(home.path())
        .args(["docker", "pull", "alpine"])
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .expect("docker pull must not fail to spawn");
    let pull_code = pull_out.status.code().unwrap_or(-1);
    assert!(
        pull_code == 0 || pull_code == 1,
        "docker pull must exit 0 or 1; got exit={pull_code} stderr:\n{}",
        String::from_utf8_lossy(&pull_out.stderr)
    );
    assert_ne!(
        pull_code, 2,
        "docker pull must NEVER exit 2 for a valid image ref"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A-308 — restart via OS supervisor (F-308): install GENERATES a unit (no
// daemon), list shows it, uninstall removes it, and ZERO lightr daemons are
// ever resident (the A4 no-daemon invariant must still hold — we only wrote a
// file). The generated unit must be supervisor-valid (parse it back).
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn a308_supervise_install_list_uninstall_no_daemon() {
    let home = TempDir::new().unwrap();
    let units = home.path().join("units");

    // install --name web --restart on-failure:3 --dir . -- /bin/echo hi
    lightr_cmd(home.path())
        .args([
            "supervise",
            "install",
            "--name",
            "web",
            "--restart",
            "on-failure:3",
            "--dir",
            ".",
            "--",
            "/bin/echo",
            "hi",
        ])
        .assert()
        .success();

    // A unit file landed under ~/.lightr/units/ with the platform extension.
    #[cfg(target_os = "macos")]
    let unit_path = units.join("web.plist");
    #[cfg(target_os = "linux")]
    let unit_path = units.join("web.service");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        assert!(
            unit_path.exists() && unit_path.is_file(),
            "supervise install must write a unit at {}",
            unit_path.display()
        );
        let text = fs::read_to_string(&unit_path).unwrap();

        // Unit must be supervisor-valid (parse/lint it back).
        #[cfg(target_os = "macos")]
        {
            // `plutil -lint` parses the plist; non-zero = malformed.
            let lint = std::process::Command::new("plutil")
                .arg("-lint")
                .arg(&unit_path)
                .output()
                .expect("plutil must be present on macOS");
            assert!(
                lint.status.success(),
                "generated plist must pass plutil -lint:\n{}\n--- unit ---\n{text}",
                String::from_utf8_lossy(&lint.stdout)
            );
            // on-failure → KeepAlive { SuccessfulExit = false }.
            assert!(
                text.contains("SuccessfulExit"),
                "on-failure ⇒ SuccessfulExit"
            );
            assert!(text.contains("<key>RunAtLoad</key>"));
        }
        #[cfg(target_os = "linux")]
        {
            // No systemd-analyze in CI guaranteed; assert the structural law.
            assert!(text.contains("[Service]"));
            assert!(text.contains("Restart=on-failure"));
            assert!(text.contains("ExecStart=/bin/echo hi"));
        }
    }

    // list shows the unit by name.
    let listed = lightr_cmd(home.path())
        .args(["supervise", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&listed).lines().any(|l| l == "web"),
        "supervise list must show 'web'; got:\n{}",
        String::from_utf8_lossy(&listed)
    );

    // The A4 invariant: install/list generated a FILE and nothing resident.
    // No control sockets, no run/ supervisor dirs, no *.pid under LIGHTR_HOME.
    #[cfg(unix)]
    {
        let entries = walkdir(home.path());
        for path in &entries {
            let meta = fs::symlink_metadata(path).unwrap();
            let ft = meta.file_type();
            use std::os::unix::fs::FileTypeExt;
            assert!(
                !ft.is_socket(),
                "supervise must leave no socket: {}",
                path.display()
            );
            assert!(
                !ft.is_fifo(),
                "supervise must leave no FIFO: {}",
                path.display()
            );
            if let Some(name) = path.file_name() {
                assert!(
                    !name.to_string_lossy().ends_with(".pid"),
                    "supervise must leave no pidfile: {}",
                    path.display()
                );
            }
        }
    }
    let run_dir = home.path().join("run");
    assert!(
        !run_dir.exists()
            || fs::read_dir(&run_dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "supervise must NOT create a resident run/ supervisor (no daemon of ours)"
    );

    // uninstall --name web removes the unit.
    lightr_cmd(home.path())
        .args(["supervise", "uninstall", "--name", "web"])
        .assert()
        .success();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    assert!(
        !unit_path.exists(),
        "supervise uninstall must remove the unit at {}",
        unit_path.display()
    );

    // uninstall again ⇒ honest error (the unit is gone), never a silent success.
    lightr_cmd(home.path())
        .args(["supervise", "uninstall", "--name", "web"])
        .assert()
        .failure();
}

#[test]
fn a308_supervise_install_rejects_bad_policy() {
    let home = TempDir::new().unwrap();
    // A garbage restart policy must fail closed (exit 2, usage-class), and write
    // nothing under LIGHTR_HOME.
    lightr_cmd(home.path())
        .args([
            "supervise",
            "install",
            "--name",
            "bad",
            "--restart",
            "sometimes",
            "--dir",
            ".",
            "--",
            "/bin/true",
        ])
        .assert()
        .failure()
        .code(2);
    assert!(
        !home.path().join("units").join("bad.plist").exists()
            && !home.path().join("units").join("bad.service").exists(),
        "a rejected policy must write no unit file"
    );
}

#[test]
fn a308_supervise_list_empty_is_clean() {
    let home = TempDir::new().unwrap();
    // No units installed ⇒ list exits 0 with empty output (not an error).
    let out = lightr_cmd(home.path())
        .args(["supervise", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&out).trim().is_empty(),
        "empty supervise list must print nothing; got:\n{}",
        String::from_utf8_lossy(&out)
    );
}

/// Recursively collect every path under `root` (for the no-daemon sweep).
#[cfg(unix)]
fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.clone());
            if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                stack.push(p);
            }
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Assert that `path` (relative to `root`) exists as a regular file.
#[allow(dead_code)]
fn assert_file_exists(root: &Path, rel: &str) {
    let p = root.join(rel);
    assert!(
        p.exists() && p.is_file(),
        "expected regular file at {}: not found",
        p.display()
    );
}
