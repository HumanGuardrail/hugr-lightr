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

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::bench_compare::{dur_ms, SAMPLES};

pub(crate) mod probes;

pub(crate) use probes::{
    build_ms, cold_image_ms, cold_run_ms, install_footprint_mb, materialize_ms, re_run_ms,
};

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
pub(crate) fn unique_name(kind: &str) -> String {
    let n = NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("lightr-bench-{kind}-{}-{n}", std::process::id())
}

/// Spawn `cmd` with stdout/stderr nulled, then poll `try_wait` until it exits or
/// `timeout` elapses. Returns the wall-clock duration spawn→exit on a clean
/// success (exit status 0). On a spawn error, a non-zero exit, or a timeout (child
/// killed + reaped) returns `Err(())` — the caller maps that to an honest SKIP.
///
/// This is the ONLY way ops are run here, so every spawned op is bounded.
pub(crate) fn run_op(cmd: &mut Command, timeout: Duration) -> Result<Duration, ()> {
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
pub(crate) fn docker(docker_bin: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(docker_bin);
    c.args(args);
    c
}

/// Run a single setup op bounded by `OP_TIMEOUT`; returns `true` on clean success.
/// Setup is UNTIMED for the result, but still bounded (tense law).
pub(crate) fn setup_ok(docker_bin: &Path, args: &[&str]) -> bool {
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
pub(crate) fn sample_median<F>(
    skip_reason: &'static str,
    mut op: F,
) -> Result<Duration, &'static str>
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
pub(crate) fn median_outcome(r: Result<Duration, &'static str>) -> Outcome {
    match r {
        Ok(d) => Outcome::Measured(dur_ms(d)),
        Err(reason) => Outcome::Skip(reason),
    }
}

/// The tiny image every run/re-run probe uses.
pub(crate) const TINY_IMAGE: &str = "alpine:latest";

/// The real image the `cold-image` probe pulls FROM COLD. MUST be DISTINCT from
/// `TINY_IMAGE`: the cold-image probe deletes it per-sample to force a genuine
/// re-fetch+extract, so sharing `TINY_IMAGE` would sabotage the run/re-run probes
/// that depend on `ensure_tiny_image` keeping that image present.
pub(crate) const COLD_IMAGE_REF: &str = "busybox:latest";

/// Ensure `TINY_IMAGE` is present (UNTIMED setup): inspect it; if absent, pull it;
/// if the pull fails AND it is still absent → `Err` (→ honest SKIP). Both the
/// inspect and the pull are bounded by `OP_TIMEOUT`.
pub(crate) fn ensure_tiny_image(docker_bin: &Path) -> Result<(), &'static str> {
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

#[cfg(test)]
mod tests;
