//! Real competitor (Docker) head-to-head probes for `bench-compare` (WP-D1).
//!
//! These functions **spawn the competitor runtime**. They are invoked ONLY from
//! the real `bench-compare` CLI entry under [`ProbePolicy::Spawn`] — NEVER from
//! tests or CI (the spawn-guard lives in `bench_compare::run_workload`: a present
//! competitor under `NeverSpawn` is an honest `SKIP`, so `cargo test` can never
//! launch a container even on a docker-equipped runner).
//!
//! ## Tense law (inviolable — ADR-0012, performance-bar.md)
//! Every spawned op is bounded by a timeout. A timeout, a setup failure, or an
//! op a present runtime cannot perform yields an honest [`Outcome::Skip`] with a
//! STATIC reason — NEVER a fabricated or guessed number.
//!
//! ## Fairness doctrine (frozen by the lead — build-spec-parity.md §7)
//! Each probe measures Docker's IDIOMATIC command for the SAME user-goal the
//! Lightr side is measured on, over the SAME fixture bytes (the probes reuse the
//! `pub(crate)` fixture builders from `bench_compare`, so the bytes are identical
//! by construction). **Setup is UNTIMED** (image build / pull / create); only the
//! user-goal op is timed, median-of-N after one warmup — mirroring the Lightr
//! side's methodology (`median_of`). The exact command each probe runs is
//! documented on the function and surfaced in the methodology doc.
//!
//! ## Timeout discipline (no `wait_timeout`, no new dependency)
//! There is no `wait_timeout` in std and we add no crate for it. [`run_op`] spawns
//! the child with stdout/stderr nulled, then polls [`std::process::Child::try_wait`]
//! in a short sleep loop until a wall-clock deadline; on the deadline it kills and
//! reaps the child and reports failure. Every spawned op — setup or timed — goes
//! through `run_op`, bounded by [`OP_TIMEOUT`] (or [`SETUP_TIMEOUT`] for the heavy
//! 1 GB materialize setup). The timed value is the wall-clock `Instant` around
//! spawn→completion.
//!
//! ## Fallible sampling
//! `bench_compare::median_of` takes an INFALLIBLE `FnMut() -> Duration`, so it
//! cannot express a docker op that failed. The timed ops here use [`sample_median`]
//! instead: one warmup (failure → SKIP), then `SAMPLES` timed runs (ANY failure →
//! SKIP), then the median of the sorted samples. Warmup + median-of-N mirrors the
//! Lightr side.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::bench_compare::{dur_ms, MaterializeSize, SAMPLES};

/// One competitor measurement. `Skip` carries a STATIC reason so it maps directly
/// onto `bench_compare::Cell::Skip(&'static str)` with no allocation.
pub(crate) enum Outcome {
    /// A real measured value in the row's unit (ms, or MB for install footprint).
    Measured(f64),
    /// Honest skip — absent op, timeout, or setup failure. Never a guess.
    Skip(&'static str),
}

/// Hard wall-clock ceiling on any single spawned docker op (run / build-warm /
/// cp / inspect / pull / create / rm). On the deadline the child is killed and the
/// op is treated as a failure (→ honest SKIP), never a fabricated number. 120 s so
/// the slow timed op — a 1 GB `docker cp` across the Mac VM (~50 s measured) — has
/// safe headroom and never false-times-out; cheap ops (run/build) finish in seconds.
pub(crate) const OP_TIMEOUT: Duration = Duration::from_secs(120);

/// Hard wall-clock ceiling on the materialize SETUP (build the 1 GB host tree +
/// `docker create` + copy the tree INTO the container). Exceeding it → honest SKIP.
pub(crate) const SETUP_TIMEOUT: Duration = Duration::from_secs(180);

/// Per-process counter for unique resource names (NOT timestamps — a clock can
/// repeat under load; a monotonic counter cannot collide within this process, and
/// `std::process::id()` separates concurrent processes).
static NAME_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A unique, docker-legal resource name for `kind` (e.g. an image tag or a build
/// context dir-name), namespaced by this process id and a monotonic counter.
fn unique_name(kind: &str) -> String {
    let n = NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("lightr-bench-{kind}-{}-{n}", std::process::id())
}

/// Spawn `cmd` with stdout/stderr nulled, then poll `try_wait` until it exits or
/// `timeout` elapses. Returns the wall-clock duration spawn→exit on a clean
/// success (exit status 0). On a spawn error, a non-zero exit, or a timeout (child
/// killed + reaped) returns `Err(())` — the caller maps that to an honest SKIP.
///
/// This is the ONLY way ops are run here, so every spawned op is bounded.
fn run_op(cmd: &mut Command, timeout: Duration) -> Result<Duration, ()> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let start = Instant::now();
    let mut child = cmd.spawn().map_err(|_| ())?;
    let poll = Duration::from_millis(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let elapsed = start.elapsed();
                return if status.success() {
                    Ok(elapsed)
                } else {
                    Err(())
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    // Deadline blown: kill + reap so we never leak a child, and
                    // report failure — a timed-out op is a SKIP, never a number.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(());
                }
                std::thread::sleep(poll);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    }
}

/// Build a `docker` `Command` for `args` against the resolved binary path.
fn docker(docker_bin: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(docker_bin);
    c.args(args);
    c
}

/// Run a single setup op bounded by `OP_TIMEOUT`; returns `true` on clean success.
/// Setup is UNTIMED for the result, but still bounded (tense law).
fn setup_ok(docker_bin: &Path, args: &[&str]) -> bool {
    run_op(&mut docker(docker_bin, args), OP_TIMEOUT).is_ok()
}

/// The fallible, identical-methodology timed sampler. `op` performs ONE timed
/// docker op and returns its duration (or `Err` on spawn/exit/timeout failure).
///
/// - one warmup run; if it fails → `Err(skip_reason)`,
/// - then `SAMPLES` timed runs; if ANY fails → `Err(skip_reason)`,
/// - else sort the durations and take the median (index `n / 2`).
///
/// Mirrors `bench_compare::median_of` (warmup + median-of-N) but is FALLIBLE, so a
/// failed docker op becomes an honest SKIP rather than a fabricated 0.
fn sample_median<F>(skip_reason: &'static str, mut op: F) -> Result<Duration, &'static str>
where
    F: FnMut() -> Result<Duration, ()>,
{
    // Warmup (untimed) — its failure means we cannot honestly measure at all.
    op().map_err(|_| skip_reason)?;
    let mut samples: Vec<Duration> = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        samples.push(op().map_err(|_| skip_reason)?);
    }
    samples.sort();
    Ok(samples[SAMPLES / 2])
}

/// Convert a `sample_median` result into an `Outcome` (ms on success, SKIP on the
/// static reason).
fn median_outcome(r: Result<Duration, &'static str>) -> Outcome {
    match r {
        Ok(d) => Outcome::Measured(dur_ms(d)),
        Err(reason) => Outcome::Skip(reason),
    }
}

/// The tiny image every run/re-run probe uses.
const TINY_IMAGE: &str = "alpine:latest";

/// The real image the `cold-image` probe pulls FROM COLD. MUST be DISTINCT from
/// `TINY_IMAGE`: the cold-image probe deletes it per-sample to force a genuine
/// re-fetch+extract, so sharing `TINY_IMAGE` would sabotage the run/re-run probes
/// that depend on `ensure_tiny_image` keeping that image present.
pub(crate) const COLD_IMAGE_REF: &str = "busybox:latest";

/// Ensure `TINY_IMAGE` is present (UNTIMED setup): inspect it; if absent, pull it;
/// if the pull fails AND it is still absent → `Err` (→ honest SKIP). Both the
/// inspect and the pull are bounded by `OP_TIMEOUT`.
fn ensure_tiny_image(docker_bin: &Path) -> Result<(), &'static str> {
    if setup_ok(docker_bin, &["image", "inspect", TINY_IMAGE]) {
        return Ok(());
    }
    // Absent (or inspect failed): try the idiomatic pull, then re-check presence.
    let _ = setup_ok(docker_bin, &["pull", TINY_IMAGE]);
    if setup_ok(docker_bin, &["image", "inspect", TINY_IMAGE]) {
        Ok(())
    } else {
        Err("tiny image unavailable (docker pull failed)")
    }
}

/// Indicator #8 — cold-run: run a trivial container once. Docker's idiomatic
/// path is `docker run --rm <tiny-image> true` (image ensured-present in setup).
/// Returns the timed run median in ms.
pub(crate) fn cold_run_ms(docker_bin: &Path, _scratch: &Path) -> Outcome {
    // SETUP (untimed): ensure the tiny image is present.
    if let Err(reason) = ensure_tiny_image(docker_bin) {
        return Outcome::Skip(reason);
    }
    // TIMED: the cost to run a trivial container once.
    median_outcome(sample_median(
        "docker op failed/timed out during sampling",
        || {
            run_op(
                &mut docker(docker_bin, &["run", "--rm", TINY_IMAGE, "true"]),
                OP_TIMEOUT,
            )
        },
    ))
}

/// Indicator #4 — re-run: run the SAME trivial job again. Docker has no memo, so
/// the idiomatic path is the SAME `docker run` repeated — it re-does the work
/// every time. Returns the steady-state run median in ms.
pub(crate) fn re_run_ms(docker_bin: &Path, _scratch: &Path) -> Outcome {
    // SETUP (untimed): same as cold_run — ensure the tiny image is present.
    if let Err(reason) = ensure_tiny_image(docker_bin) {
        return Outcome::Skip(reason);
    }
    // TIMED: the SAME `docker run` repeated — no memo, full work every time.
    median_outcome(sample_median(
        "docker op failed/timed out during sampling",
        || {
            run_op(
                &mut docker(docker_bin, &["run", "--rm", TINY_IMAGE, "true"]),
                OP_TIMEOUT,
            )
        },
    ))
}

/// Indicator #4/#8 — build a 3-step Dockerfile a SECOND time (warm layer cache),
/// the fair cache-vs-memo race. The Lightr side uses `FROM scratch` + `RUN` (valid
/// for Lightr's builder), which docker CANNOT build (scratch has no shell for
/// `RUN`); so the docker side builds an equivalent `FROM alpine` 3-step context.
/// Both measure the cached 2nd-build overhead — the indicator. Returns median ms.
pub(crate) fn build_ms(docker_bin: &Path, scratch: &Path) -> Outcome {
    // SETUP (untimed): ensure the base image, then write a docker-buildable 3-step
    // context (FROM alpine, mirroring the Lightr side's COPY/RUN/COPY shape).
    if let Err(reason) = ensure_tiny_image(docker_bin) {
        return Outcome::Skip(reason);
    }
    let ctx = scratch.join(unique_name("build-ctx"));
    if std::fs::create_dir_all(&ctx).is_err()
        || std::fs::write(ctx.join("fileA.txt"), b"alpha content").is_err()
        || std::fs::write(ctx.join("fileB.txt"), b"beta content").is_err()
        || std::fs::write(
            ctx.join("Dockerfile"),
            b"FROM alpine\nCOPY fileA.txt /a.txt\nRUN echo built\nCOPY fileB.txt /b.txt\n",
        )
        .is_err()
    {
        return Outcome::Skip("docker build context setup failed");
    }
    let tag = unique_name("build");
    let ctx_str = ctx.to_string_lossy().to_string();

    // SETUP (untimed): one COLD build to warm the layer cache.
    if !setup_ok(docker_bin, &["build", "-t", &tag, &ctx_str]) {
        return Outcome::Skip("docker cold build (cache warm) failed");
    }

    // TIMED: the 2nd build (warm cache hit) — the fair cache-vs-memo race.
    let out = median_outcome(sample_median(
        "docker op failed/timed out during sampling",
        || {
            run_op(
                &mut docker(docker_bin, &["build", "-t", &tag, &ctx_str]),
                OP_TIMEOUT,
            )
        },
    ));

    // Clean up the image (best-effort — cleanup failures never affect the result).
    let _ = setup_ok(docker_bin, &["rmi", "-f", &tag]);
    out
}

/// cold-image: time `docker pull` of a real image FROM COLD. Each sample first
/// removes the image (untimed intent: guarantee a real re-fetch+extract), then
/// times the pull. Uses a DISTINCT image (COLD_IMAGE_REF) so it never disturbs
/// the shared TINY_IMAGE the other probes depend on. Tense law: any failure → Skip.
pub(crate) fn cold_image_ms(docker_bin: &Path, _scratch: &Path) -> Outcome {
    let out = median_outcome(sample_median(
        "docker pull (cold-image) failed or timed out during sampling",
        || {
            // Force cold: drop the image so the next pull genuinely re-fetches.
            let _ = run_op(
                &mut docker(docker_bin, &["rmi", "-f", COLD_IMAGE_REF]),
                OP_TIMEOUT,
            );
            run_op(
                &mut docker(docker_bin, &["pull", COLD_IMAGE_REF]),
                OP_TIMEOUT,
            )
        },
    ));
    // Best-effort cleanup so we don't leave the image on the operator's box.
    let _ = run_op(
        &mut docker(docker_bin, &["rmi", "-f", COLD_IMAGE_REF]),
        OP_TIMEOUT,
    );
    out
}

/// Indicator #3 — materialize a 1 GB tree into a usable host directory. Lightr
/// uses `clonefile` CoW from CAS; the fair Docker mirror is `docker cp
/// <container>:/data <dest>` — a full byte copy of the SAME 1 GB across the Mac
/// VM. SETUP (untimed): build the 1 GB host tree, `docker create` a container, and
/// copy the tree INTO it (so the bytes live in docker's container fs, mirroring
/// "bytes already in CAS"). We deliberately do NOT `docker build` a 1 GB image —
/// sending a 1 GB build context to the VM costs minutes (measured: ~7 min, blows
/// the budget); the cp-in is the faster, fairer ingest and fits the budget.
/// Returns the timed median (the cp-OUT) in ms.
pub(crate) fn materialize_ms(docker_bin: &Path, scratch: &Path, size: MaterializeSize) -> Outcome {
    // SETUP (untimed, bounded by SETUP_TIMEOUT): base image + the SAME 1 GB tree.
    if let Err(reason) = ensure_tiny_image(docker_bin) {
        return Outcome::Skip(reason);
    }
    let setup_start = Instant::now();
    let tree = scratch.join(unique_name("mat-tree"));
    if std::fs::create_dir_all(&tree).is_err() {
        return Outcome::Skip("docker materialize setup failed");
    }
    if super::bench_compare::build_materialize_fixture(&tree, size).is_err() {
        return Outcome::Skip("docker materialize fixture build failed");
    }

    // A stopped container we can cp into/out of (`docker cp` works on a stopped
    // container's fs; no need to start it).
    let cid = match create_container(docker_bin, TINY_IMAGE) {
        Some(cid) => cid,
        None => return Outcome::Skip("docker create (materialize) failed"),
    };

    // Copy the 1 GB tree INTO the container (untimed ingest, bounded by the
    // remaining setup budget) — docker's "get the bytes into the store".
    let tree_str = tree.to_string_lossy().to_string();
    let into = format!("{cid}:/data");
    let ingest_budget = SETUP_TIMEOUT.saturating_sub(setup_start.elapsed());
    if run_op(
        &mut docker(docker_bin, &["cp", &tree_str, &into]),
        ingest_budget,
    )
    .is_err()
    {
        let _ = setup_ok(docker_bin, &["rm", "-f", &cid]);
        return Outcome::Skip("docker materialize ingest exceeded budget");
    }

    // TIMED: extract the 1 GB tree to a FRESH host dir each sample —
    // `docker cp <cid>:/data <dest>`, the full byte copy across the VM (mirrors
    // Lightr's clonefile hydrate). Fresh dest per sample so no copy is a no-op.
    let dest_base = scratch.join(unique_name("mat-dest"));
    let _ = std::fs::create_dir_all(&dest_base);
    let mut counter = 0usize;
    let src = format!("{cid}:/data");
    let out = median_outcome(sample_median(
        "docker op failed/timed out during sampling",
        || {
            counter += 1;
            let dest = dest_base.join(format!("d{counter}"));
            let dest_str = dest.to_string_lossy().to_string();
            run_op(
                &mut docker(docker_bin, &["cp", &src, &dest_str]),
                OP_TIMEOUT,
            )
        },
    ));

    // Clean up the container (best-effort).
    let _ = setup_ok(docker_bin, &["rm", "-f", &cid]);
    out
}

/// `docker create <tag>` bounded by `OP_TIMEOUT`, capturing the container id from
/// stdout. Returns the trimmed cid on a clean success, `None` on any failure /
/// timeout / empty id. Unlike `run_op` this captures stdout (the cid is the goal),
/// but it polls `try_wait` against the SAME deadline so it is still bounded.
fn create_container(docker_bin: &Path, tag: &str) -> Option<String> {
    let mut child = docker(docker_bin, &["create", tag])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    let poll = Duration::from_millis(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if start.elapsed() >= OP_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(poll);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let cid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if cid.is_empty() {
        None
    } else {
        Some(cid)
    }
}

/// Indicator #1 — install footprint. Lightr is its single static binary; Docker
/// is the installed `Docker.app` bundle on disk. We sum the regular-file sizes
/// under the bundle (a `du`-style measure, symlinks NOT followed) — a REAL
/// measurement, NOT a container spawn (so this probe is honest even before the
/// spawn probes land). Returns MB. SKIP (never a guess) if the bundle can't be
/// located or its root can't be read.
pub(crate) fn install_footprint_mb(docker_bin: &Path) -> Outcome {
    for cand in docker_app_candidates(docker_bin) {
        if cand.is_dir() {
            if let Some(bytes) = dir_size_bytes(&cand) {
                return Outcome::Measured(bytes as f64 / (1024.0 * 1024.0));
            }
        }
    }
    Outcome::Skip("Docker.app bundle not located on disk")
}

/// Candidate `Docker.app` bundle locations: the standard `/Applications`, a
/// user-local `~/Applications`, and any `*.app` ancestor of the resolved binary
/// (Docker Desktop's CLI shim lives under the bundle). First existing dir wins.
fn docker_app_candidates(docker_bin: &Path) -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from("/Applications/Docker.app")];
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home).join("Applications/Docker.app"));
    }
    let mut cur = docker_bin;
    while let Some(parent) = cur.parent() {
        if parent.extension().and_then(|e| e.to_str()) == Some("app") {
            v.push(parent.to_path_buf());
            break;
        }
        cur = parent;
    }
    v
}

/// Sum of regular-file sizes under `root`, NOT following symlinks (du-style).
/// Unreadable subdirectories are skipped (best-effort, never panics); `None`
/// only if `root` itself cannot be read (→ honest SKIP upstream).
fn dir_size_bytes(root: &Path) -> Option<u64> {
    // The root must be readable; deeper unreadable dirs are skipped honestly.
    std::fs::read_dir(root).ok()?;
    let mut total: u64 = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(md) = entry.path().symlink_metadata() else {
                continue;
            };
            let ft = md.file_type();
            if ft.is_symlink() {
                continue;
            } else if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total = total.saturating_add(md.len());
            }
        }
    }
    Some(total)
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
