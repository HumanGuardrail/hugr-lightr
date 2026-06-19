use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use super::super::bench_compare::MaterializeSize;
use super::{
    docker, ensure_tiny_image, median_outcome, run_op, sample_median, setup_ok, unique_name,
    Outcome, COLD_IMAGE_REF, OP_TIMEOUT, SETUP_TIMEOUT, TINY_IMAGE,
};

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
pub(crate) fn materialize_ms(
    docker_bin: &Path,
    scratch: &Path,
    size: MaterializeSize,
) -> Outcome {
    // SETUP (untimed, bounded by SETUP_TIMEOUT): base image + the SAME 1 GB tree.
    if let Err(reason) = ensure_tiny_image(docker_bin) {
        return Outcome::Skip(reason);
    }
    let setup_start = Instant::now();
    let tree = scratch.join(unique_name("mat-tree"));
    if std::fs::create_dir_all(&tree).is_err() {
        return Outcome::Skip("docker materialize setup failed");
    }
    if super::super::bench_compare::build_materialize_fixture(&tree, size).is_err() {
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
pub(crate) fn create_container(docker_bin: &Path, tag: &str) -> Option<String> {
    let mut child = docker(docker_bin, &["create", tag])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    let poll = std::time::Duration::from_millis(20);
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
pub(crate) fn docker_app_candidates(docker_bin: &Path) -> Vec<PathBuf> {
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
pub(crate) fn dir_size_bytes(root: &Path) -> Option<u64> {
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
