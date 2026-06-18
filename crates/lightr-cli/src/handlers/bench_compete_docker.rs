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
//! WP-D1 fills these bodies. Until then each returns an honest SKIP so the seam
//! compiles and the table renders truthfully (docker cells SKIP, not fabricated).

use std::path::{Path, PathBuf};

use super::bench_compare::MaterializeSize;

/// One competitor measurement. `Skip` carries a STATIC reason so it maps directly
/// onto `bench_compare::Cell::Skip(&'static str)` with no allocation.
pub(crate) enum Outcome {
    /// A real measured value in the row's unit (ms, or MB for install footprint).
    Measured(f64),
    /// Honest skip — absent op, timeout, or setup failure. Never a guess.
    Skip(&'static str),
}

/// The reason emitted by every stub until WP-D1 lands. Distinct from the
/// tense-law guard skip so the table reader can tell "not implemented yet" from
/// "spawn disabled in this context".
const STUB: &str = "docker head-to-head not yet implemented (WP-D1)";

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

/// Indicator #8 — cold-run: run a trivial container once. Docker's idiomatic
/// path is `docker run --rm <tiny-image> true` (image ensured-present in setup).
/// Returns the timed run median in ms.
pub(crate) fn cold_run_ms(_docker_bin: &Path, _scratch: &Path) -> Outcome {
    Outcome::Skip(STUB)
}

/// Indicator #4 — re-run: run the SAME trivial job again. Docker has no memo, so
/// the idiomatic path is the SAME `docker run` repeated — it re-does the work
/// every time. Returns the steady-state run median in ms.
pub(crate) fn re_run_ms(_docker_bin: &Path, _scratch: &Path) -> Outcome {
    Outcome::Skip(STUB)
}

/// Indicator #4/#8 — build the same 3-step Dockerfile a SECOND time (warm layer
/// cache). Docker's idiomatic path is `docker build` over an identical context
/// (built via the shared `make_bench_dockerfile`). Returns the cached-build
/// median in ms.
pub(crate) fn build_ms(_docker_bin: &Path, _scratch: &Path) -> Outcome {
    Outcome::Skip(STUB)
}

/// Indicator #3 — materialize a 1 GB tree into a usable host directory. Lightr
/// uses `clonefile` CoW from CAS; Docker's idiomatic host-side path is
/// `docker cp <container>:/data <dest>` from a container carrying the SAME bytes
/// (built via the shared `build_materialize_fixture`). Returns the timed median
/// in ms. SETUP (untimed): build/load the 1 GB-layer image + `docker create`.
pub(crate) fn materialize_ms(
    _docker_bin: &Path,
    _scratch: &Path,
    _size: MaterializeSize,
) -> Outcome {
    Outcome::Skip(STUB)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests — portable, NO container spawn. (The spawn probes are exercised by the
// operator at marketing time on a real box; their command construction is
// reviewed, not unit-run, so `cargo test`/CI never launch a container.)
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
}
