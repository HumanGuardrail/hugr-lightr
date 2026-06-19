//! One measurement helper per `// ── BN ──` benchmark block.
//!
//! Each function takes exactly the context it needs and returns one or more
//! `BenchRow` values in the same order the orchestrator in `mod.rs` pushes them.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use lightr_engine::{probe as engine_probe, EngineKind};
use lightr_index::{hydrate, snapshot, status};
use lightr_store::Store;

use super::fixture::{
    make_bench_compose, make_bench_dockerfile, make_tiny_oci_tar, median_of, time_spawn,
};
use super::{
    BenchRow, Row, SkipRow, BUDGET_BUILD_CACHED_MS, BUDGET_BUILD_COLD_MS, BUDGET_COMPOSE_UP_MS,
    BUDGET_HIT_RUN_MS, BUDGET_HYDRATE_MS, BUDGET_MICROWAVE_FLOOR_MS, BUDGET_OCI_IMPORT_MS,
    BUDGET_REPLAY_MS, BUDGET_SNAPSHOT_COLD_MS, BUDGET_SNAPSHOT_WARM_MS, BUDGET_STATUS_WARM_MS,
    BUDGET_VERSION_MS,
};

// ── B1: version overhead (spawn self --version) ────────────────────────────
pub(super) fn b1_version() -> BenchRow {
    let b1 = median_of(|| time_spawn(&["--version"]), 5);
    BenchRow::Measured(Row::new("B1 version", b1, BUDGET_VERSION_MS, true))
}

// ── B5a + B5b: snapshot cold then warm ────────────────────────────────────
pub(super) fn b5a_b5b_snapshot(
    fixture_root: &Path,
    store_root: &Path,
    lightr_home: &Path,
    open_store: &dyn Fn() -> Store,
) -> Vec<BenchRow> {
    // B5a: cold
    let snap_cold_dur = {
        let mut samples: Vec<std::time::Duration> = Vec::with_capacity(5);
        // 1 warmup
        {
            let s = open_store();
            let _ = snapshot(fixture_root, &s, "bench-cold").ok();
        }
        // wipe the store between cold samples so index doesn't warm up
        for _ in 0..5 {
            let obj_dir = store_root.join("objects");
            if obj_dir.exists() {
                fs::remove_dir_all(&obj_dir).ok();
            }
            let idx_dir = lightr_home.join("index");
            if idx_dir.exists() {
                fs::remove_dir_all(&idx_dir).ok();
            }
            let s = open_store();
            let t = Instant::now();
            let _ = snapshot(fixture_root, &s, "bench-cold").ok();
            samples.push(t.elapsed());
        }
        samples.sort();
        samples[2]
    };

    // B5b: warm (index populated from B5a above)
    let snap_warm_dur = {
        let s = open_store();
        // ensure warm by doing one more snap.
        let _ = snapshot(fixture_root, &s, "bench-warm").ok();
        median_of(
            || {
                let ss = open_store();
                let t = Instant::now();
                let _ = snapshot(fixture_root, &ss, "bench-warm").ok();
                t.elapsed()
            },
            5,
        )
    };

    vec![
        BenchRow::Measured(Row::new(
            "B5a snapshot-cold",
            snap_cold_dur,
            BUDGET_SNAPSHOT_COLD_MS,
            false,
        )),
        BenchRow::Measured(Row::new(
            "B5b snapshot-warm",
            snap_warm_dur,
            BUDGET_SNAPSHOT_WARM_MS,
            false,
        )),
    ]
}

// ── B6: status warm-index ────────────────────────────────────────────────
pub(super) fn b6_status(fixture_root: &Path, open_store: &dyn Fn() -> Store) -> BenchRow {
    let status_warm_dur = {
        let s = open_store();
        // ensure snapshot exists.
        let _ = snapshot(fixture_root, &s, "bench-status").ok();
        median_of(
            || {
                let ss = open_store();
                let t = Instant::now();
                let _ = status(fixture_root, &ss, "bench-status").ok();
                t.elapsed()
            },
            5,
        )
    };
    BenchRow::Measured(Row::new(
        "B6 status-warm",
        status_warm_dur,
        BUDGET_STATUS_WARM_MS,
        false,
    ))
}

// ── B3′: hydrate ──────────────────────────────────────────────────────────
pub(super) fn b3_hydrate(fixture_root: &Path, open_store: &dyn Fn() -> Store) -> BenchRow {
    // Ensure objects are in store first.
    {
        let s = open_store();
        let _ = snapshot(fixture_root, &s, "bench-hydrate").ok();
    }
    let hydrate_dur = {
        let dest_tmp = tempfile::tempdir().expect("hydrate dest tempdir");
        let dest_base = dest_tmp.path().to_path_buf();
        let mut samples: Vec<std::time::Duration> = Vec::with_capacity(5);
        // warmup
        {
            let dest = dest_base.join("warmup");
            fs::create_dir_all(&dest).ok();
            let s = open_store();
            let _ = hydrate(&dest, &s, "bench-hydrate").ok();
        }
        for i in 0..5usize {
            let dest = dest_base.join(format!("run{i}"));
            fs::create_dir_all(&dest).ok();
            let s = open_store();
            let t = Instant::now();
            let _ = hydrate(&dest, &s, "bench-hydrate").ok();
            samples.push(t.elapsed());
        }
        samples.sort();
        samples[2]
    };
    BenchRow::Measured(Row::new(
        "B3' hydrate",
        hydrate_dur,
        BUDGET_HYDRATE_MS,
        false,
    ))
}

// ── B2/B4: run memo MISS then HIT (echo fixture path) ──────────────────
pub(super) fn b2_b4_run_memo(fixture_root: &Path, store_root: &Path) -> Vec<BenchRow> {
    let echo_arg = fixture_root.to_string_lossy().to_string();

    // MISS (first run, cold AC).
    {
        let ac_dir = store_root.join("ac");
        if ac_dir.exists() {
            fs::remove_dir_all(&ac_dir).ok();
        }
    }
    let miss_dur = median_of(
        || {
            time_spawn(&[
                "run",
                "--dir",
                &echo_arg,
                "--",
                "echo",
                "lightr-bench-fixture",
            ])
        },
        5,
    );

    // HIT (second run, AC populated).
    let hit_dur = median_of(
        || {
            time_spawn(&[
                "run",
                "--dir",
                &echo_arg,
                "--",
                "echo",
                "lightr-bench-fixture",
            ])
        },
        5,
    );

    vec![
        BenchRow::Measured(Row::new("B4 replay/miss", miss_dur, BUDGET_REPLAY_MS, true)),
        BenchRow::Measured(Row::new("B2 hit-run", hit_dur, BUDGET_HIT_RUN_MS, true)),
    ]
}

// ── B9: oci-import (tiny in-mem docker-save tar) ────────────────────────
pub(super) fn b9_oci_import() -> BenchRow {
    let b9_img_dir = tempfile::tempdir().expect("b9 img tempdir");
    let tar_path = make_tiny_oci_tar(b9_img_dir.path());
    let tar_str = tar_path.to_string_lossy().to_string();

    let b9_dur = median_of(
        || {
            let home_tmp = tempfile::tempdir().expect("b9 home tmpdir");
            let exe = std::env::current_exe().expect("current_exe");
            let t = Instant::now();
            let _out = Command::new(&exe)
                .env("LIGHTR_HOME", home_tmp.path())
                .args(["oci", "import", &tar_str, "--name", "bench-oci"])
                .output()
                .expect("spawn oci import");
            t.elapsed()
        },
        5,
    );
    BenchRow::Measured(Row::new(
        "B9 oci-import",
        b9_dur,
        BUDGET_OCI_IMPORT_MS,
        true,
    ))
}

// ── B10a/B10: build cold then build cached ──────────────────────────────
pub(super) fn b10_build() -> Vec<BenchRow> {
    let build_ctx_dir = tempfile::tempdir().expect("build ctx tempdir");
    make_bench_dockerfile(build_ctx_dir.path());
    let ctx_str = build_ctx_dir.path().to_string_lossy().to_string();

    // B10a: cold build (1 sample — expensive; not a median)
    let b10a_dur = {
        let home_tmp = tempfile::tempdir().expect("b10a home tmpdir");
        let exe = std::env::current_exe().expect("current_exe");
        let t = Instant::now();
        let _out = Command::new(&exe)
            .env("LIGHTR_HOME", home_tmp.path())
            .args(["build", &ctx_str, "-t", "bench-build-cold"])
            .output()
            .expect("spawn build cold");
        t.elapsed()
    };

    // B10: cached build — reuse same home so AC is warm
    let b10_home = tempfile::tempdir().expect("b10 home tempdir");
    // warm-up run (populates AC)
    {
        let exe = std::env::current_exe().expect("current_exe");
        let _out = Command::new(&exe)
            .env("LIGHTR_HOME", b10_home.path())
            .args(["build", &ctx_str, "-t", "bench-build-warm"])
            .output()
            .expect("spawn build warm-up");
    }
    let b10_dur = median_of(
        || {
            let exe = std::env::current_exe().expect("current_exe");
            let t = Instant::now();
            let _out = Command::new(&exe)
                .env("LIGHTR_HOME", b10_home.path())
                .args(["build", &ctx_str, "-t", "bench-build-warm"])
                .output()
                .expect("spawn build cached");
            t.elapsed()
        },
        3,
    );

    vec![
        BenchRow::Measured(Row::new(
            "B10a build-cold",
            b10a_dur,
            BUDGET_BUILD_COLD_MS,
            true,
        )),
        BenchRow::Measured(Row::new(
            "B10 build-cached",
            b10_dur,
            BUDGET_BUILD_CACHED_MS,
            true,
        )),
    ]
}

// ── B11: compose-up (1-service, high port) ──────────────────────────────
pub(super) fn b11_compose() -> BenchRow {
    let compose_ctx_dir = tempfile::tempdir().expect("compose ctx tempdir");
    let compose_file = make_bench_compose(compose_ctx_dir.path());
    let compose_str = compose_file.to_string_lossy().to_string();

    let b11_home = tempfile::tempdir().expect("b11 home tempdir");
    let b11_dur = {
        let exe = std::env::current_exe().expect("current_exe");
        let t = Instant::now();
        let _out = Command::new(&exe)
            .env("LIGHTR_HOME", b11_home.path())
            .args(["compose", "up", "-f", &compose_str])
            .output()
            .expect("spawn compose up");
        t.elapsed()
    };

    // Tear down to clean up any supervisor processes
    {
        let exe = std::env::current_exe().expect("current_exe");
        let _out = Command::new(&exe)
            .env("LIGHTR_HOME", b11_home.path())
            .args(["compose", "down"])
            .output()
            .ok();
    }

    BenchRow::Measured(Row::new(
        "B11 compose-up",
        b11_dur,
        BUDGET_COMPOSE_UP_MS,
        true,
    ))
}

// ── B12: microwave-floor (cold vz container run) ────────────────────────
//
// TENSE LAW: we NEVER fabricate a number. Vz requires macOS + the 'vz' build
// feature + a linux pack on disk. When ANY of those prerequisites is absent
// (the common case incl. all CI), B12 is a SKIP row — never a measured row with
// a made-up duration. A SKIP never trips --check.
pub(super) fn b12_microwave() -> BenchRow {
    let vz_caps = engine_probe(EngineKind::Vz);
    if vz_caps.available {
        // Probe confirmed: vz engine + linux pack are present. Measure one cold
        // run (no warmup — we want the true cold wall-clock).
        let exe = std::env::current_exe().expect("current_exe");
        let b12_home = tempfile::tempdir().expect("b12 home tempdir");
        let b12_dur = {
            let t = Instant::now();
            let _out = Command::new(&exe)
                .env("LIGHTR_HOME", b12_home.path())
                .args(["run", "--engine", "vz", "--", "/bin/echo", "hi"])
                .output()
                .expect("spawn b12 vz run");
            t.elapsed()
        };
        BenchRow::Measured(Row::new(
            "B12 microwave-floor",
            b12_dur,
            BUDGET_MICROWAVE_FLOOR_MS,
            true,
        ))
    } else {
        // Vz not available (no macOS+feature+pack) — emit a SKIP row.
        // This is the expected path in CI and on any non-vz-equipped machine.
        BenchRow::Skipped(SkipRow {
            indicator: "B12 microwave-floor",
            reason: vz_caps.detail,
        })
    }
}
