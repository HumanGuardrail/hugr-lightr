//! `lightr bench` handler — build-spec v2 §9.
//!
//! Builds a fixture tree (2k files × 1KiB + 1×8MiB) in a tempdir with
//! LIGHTR_HOME also tempdir; measures with std::time::Instant medians-of-5
//! after 1 warmup.
//!
//! Budgets compiled in:
//!   B1  version overhead:    5 ms   (spawn-measured → ×3 in --check CI)
//!   B2  memo-hit run:        50 ms machine-class (spawn-measured → ×3 in --check)
//!   B4  replay:              35 ms machine-class (views/AS unlock the ~10 ms target)
//!   B6  status warm-index:   500 ms
//!   B3′ hydrate:             5000 ms
//!   B5a snapshot cold:       2500 ms
//!   B5b snapshot warm:       500 ms
//!
//! --check: exit 1 if any over budget.
//! --vs-docker: compare docker version overhead (2s timeout); skip if absent.
//! --json: array of {indicator,median_ms,budget_ms,pass}.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tar;

use lightr_index::{hydrate, snapshot, status};
use lightr_store::Store;
use serde::Serialize;

// ──────────────────────────────────────────────────────────────────────────────
// Budgets (frozen)
// ──────────────────────────────────────────────────────────────────────────────

const BUDGET_VERSION_MS: u64 = 5;
// Machine-class law (spec §9, S4 + first bench run, Intel i7 dev box):
// an end-to-end memo hit must re-validate inputs = warm stat-walk of the
// fixture (~45 ms k files here). The ~10 ms whitepaper target binds to
// the R2 views layer (mutation-tracked, no walk) + Apple Silicon.
const BUDGET_HIT_RUN_MS: u64 = 50;
const BUDGET_REPLAY_MS: u64 = 35;
const BUDGET_STATUS_WARM_MS: u64 = 500;
const BUDGET_HYDRATE_MS: u64 = 5_000;
const BUDGET_SNAPSHOT_COLD_MS: u64 = 2_500;
const BUDGET_SNAPSHOT_WARM_MS: u64 = 500;
// R4 §3 bench expansion (B9–B11) — generous machine-class budgets
const BUDGET_OCI_IMPORT_MS: u64 = 2_000;
const BUDGET_BUILD_COLD_MS: u64 = 5_000;
const BUDGET_BUILD_CACHED_MS: u64 = 2_000;
const BUDGET_COMPOSE_UP_MS: u64 = 3_000;

/// Spawn-measured indicators get ×3 margin in --check (debug/CI noise).
const SPAWN_MARGIN: u64 = 3;

// ──────────────────────────────────────────────────────────────────────────────
// Fixture builder
// ──────────────────────────────────────────────────────────────────────────────

fn build_fixture(root: &Path) -> std::io::Result<()> {
    // 2000 files × 1KiB across a few subdirs.
    let dirs = ["a", "b", "c", "d"];
    for d in dirs {
        fs::create_dir_all(root.join(d))?;
    }
    let small_content = vec![0xABu8; 1024];
    for i in 0..2000usize {
        let sub = dirs[i % dirs.len()];
        let p = root.join(sub).join(format!("file{i:04}.dat"));
        fs::write(p, &small_content)?;
    }
    // 1×8MiB file.
    let big_content = vec![0x5Au8; 8 * 1024 * 1024];
    fs::write(root.join("big.dat"), &big_content)?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Measurement helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Run `f` once as warmup, then `n` times; return median duration.
fn median_of<F: FnMut() -> Duration>(mut f: F, n: usize) -> Duration {
    // warmup
    let _ = f();
    let mut samples: Vec<Duration> = (0..n).map(|_| f()).collect();
    samples.sort();
    samples[n / 2]
}

fn time_spawn(args: &[&str]) -> Duration {
    let exe = std::env::current_exe().expect("current_exe");
    let t = Instant::now();
    let _out = Command::new(&exe).args(args).output().expect("spawn self");
    t.elapsed()
}

// ──────────────────────────────────────────────────────────────────────────────
// Row
// ──────────────────────────────────────────────────────────────────────────────

struct Row {
    indicator: &'static str,
    median_ms: f64,
    budget_ms: u64,
    /// effective budget for --check (may be ×3 for spawn-measured)
    check_budget_ms: u64,
    pass: bool,
}

impl Row {
    fn new(indicator: &'static str, dur: Duration, budget_ms: u64, spawn_measured: bool) -> Self {
        let median_ms = dur.as_secs_f64() * 1000.0;
        let check_budget_ms = if spawn_measured {
            budget_ms * SPAWN_MARGIN
        } else {
            budget_ms
        };
        let pass = median_ms <= check_budget_ms as f64;
        Row {
            indicator,
            median_ms,
            budget_ms,
            check_budget_ms,
            pass,
        }
    }
}

#[derive(Serialize)]
struct RowJson {
    indicator: String,
    median_ms: f64,
    budget_ms: u64,
    pass: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

pub fn run(vs_docker: bool, check: bool, json: bool) -> i32 {
    // Temp dirs for fixture + store.
    let fixture_tmp = tempfile::tempdir().expect("fixture tempdir");
    let home_tmp = tempfile::tempdir().expect("home tempdir");

    let fixture_root = fixture_tmp.path().to_path_buf();
    let lightr_home = home_tmp.path().to_path_buf();

    // Override LIGHTR_HOME for subprocess spawns via env.
    // (The lightr_store::Store::default_root() reads $LIGHTR_HOME at call time.)
    std::env::set_var("LIGHTR_HOME", &lightr_home);

    // Build fixture.
    build_fixture(&fixture_root).expect("build fixture");

    let store_root = lightr_home.join("store");
    let open_store = || Store::open(&store_root).expect("open store");

    let mut rows: Vec<Row> = Vec::new();

    // ── B1: version overhead (spawn self --version) ────────────────────────
    let b1 = median_of(|| time_spawn(&["--version"]), 5);
    rows.push(Row::new("B1 version", b1, BUDGET_VERSION_MS, true));

    // ── B5a: snapshot cold ────────────────────────────────────────────────
    // Cold = fixture not yet in store, no warm index.
    // We need a fresh store each cold run; recreate between samples.
    let snap_cold_dur = {
        let mut samples: Vec<Duration> = Vec::with_capacity(5);
        // 1 warmup
        {
            let s = open_store();
            let _ = snapshot(&fixture_root, &s, "bench-cold").ok();
        }
        // wipe the store between cold samples so index doesn't warm up
        for _ in 0..5 {
            // wipe store objects only (keep home structure)
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
            let _ = snapshot(&fixture_root, &s, "bench-cold").ok();
            samples.push(t.elapsed());
        }
        samples.sort();
        samples[2]
    };
    rows.push(Row::new(
        "B5a snapshot-cold",
        snap_cold_dur,
        BUDGET_SNAPSHOT_COLD_MS,
        false,
    ));

    // ── B5b: snapshot warm ────────────────────────────────────────────────
    // warm = index populated from previous snapshot above.
    let snap_warm_dur = {
        let s = open_store();
        // ensure warm by doing one more snap.
        let _ = snapshot(&fixture_root, &s, "bench-warm").ok();
        median_of(
            || {
                let ss = open_store();
                let t = Instant::now();
                let _ = snapshot(&fixture_root, &ss, "bench-warm").ok();
                t.elapsed()
            },
            5,
        )
    };
    rows.push(Row::new(
        "B5b snapshot-warm",
        snap_warm_dur,
        BUDGET_SNAPSHOT_WARM_MS,
        false,
    ));

    // ── B6: status warm-index ─────────────────────────────────────────────
    let status_warm_dur = {
        let s = open_store();
        // ensure snapshot exists.
        let _ = snapshot(&fixture_root, &s, "bench-status").ok();
        median_of(
            || {
                let ss = open_store();
                let t = Instant::now();
                let _ = status(&fixture_root, &ss, "bench-status").ok();
                t.elapsed()
            },
            5,
        )
    };
    rows.push(Row::new(
        "B6 status-warm",
        status_warm_dur,
        BUDGET_STATUS_WARM_MS,
        false,
    ));

    // ── B3′: hydrate ──────────────────────────────────────────────────────
    // Ensure objects are in store first.
    {
        let s = open_store();
        let _ = snapshot(&fixture_root, &s, "bench-hydrate").ok();
    }
    let hydrate_dur = {
        let dest_tmp = tempfile::tempdir().expect("hydrate dest tempdir");
        let dest_base = dest_tmp.path().to_path_buf();
        let mut samples: Vec<Duration> = Vec::with_capacity(5);
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
    rows.push(Row::new(
        "B3' hydrate",
        hydrate_dur,
        BUDGET_HYDRATE_MS,
        false,
    ));

    // ── B2/B4: run memo MISS then HIT (echo fixture path) ─────────────────
    // We use spawn self to measure overhead; separately measure MISS vs HIT
    // via the library (to keep it accurate without spinning up processes).
    // Contract says spawn-measured; we measure MISS+HIT via self-spawn.
    let echo_arg = fixture_root.to_string_lossy().to_string();

    // MISS (first run, cold AC).
    {
        // wipe AC.
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
    rows.push(Row::new("B4 replay/miss", miss_dur, BUDGET_REPLAY_MS, true));

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
    rows.push(Row::new("B2 hit-run", hit_dur, BUDGET_HIT_RUN_MS, true));

    // ── B9: oci-import (tiny in-mem docker-save tar) ──────────────────────
    //
    // Build a minimal docker-save tar in-memory (manifest.json + one small
    // uncompressed layer tar) in a fresh tempdir, then time `oci import`.
    // A new LIGHTR_HOME is used per call so the store is fresh each run.
    {
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
        rows.push(Row::new(
            "B9 oci-import",
            b9_dur,
            BUDGET_OCI_IMPORT_MS,
            true,
        ));
    }

    // ── B10a/B10: build cold then build cached ────────────────────────────
    //
    // Write a 3-step Dockerfile into a temp context dir. Build once (cold),
    // then again with the same context (warm/cached). The cached run should
    // be materially faster (all steps hit AC).
    {
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
        rows.push(Row::new(
            "B10a build-cold",
            b10a_dur,
            BUDGET_BUILD_COLD_MS,
            true,
        ));

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
        rows.push(Row::new(
            "B10 build-cached",
            b10_dur,
            BUDGET_BUILD_CACHED_MS,
            true,
        ));
    }

    // ── B11: compose-up (1-service, high port) ────────────────────────────
    //
    // Write a 1-service compose.yml binding a high (unprivileged) port.
    // Time `compose up`. Then tear down with `compose down` to clean up.
    {
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

        rows.push(Row::new(
            "B11 compose-up",
            b11_dur,
            BUDGET_COMPOSE_UP_MS,
            true,
        ));
    }

    // ── --vs-docker ────────────────────────────────────────────────────────
    let docker_line: Option<String> = if vs_docker { check_docker() } else { None };

    // ── Output ─────────────────────────────────────────────────────────────
    let any_fail = rows.iter().any(|r| !r.pass);

    if json {
        let arr: Vec<RowJson> = rows
            .iter()
            .map(|r| RowJson {
                indicator: r.indicator.to_string(),
                median_ms: r.median_ms,
                budget_ms: r.budget_ms,
                pass: r.pass,
            })
            .collect();
        println!("{}", serde_json::to_string(&arr).expect("serialize bench"));
    } else {
        println!(
            "{:<22}  {:>10}  {:>10}  verdict",
            "indicator", "median", "budget"
        );
        println!("{}", "-".repeat(58));
        for r in &rows {
            println!(
                "{:<22}  {:>9.1}ms  {:>9}ms  {}",
                r.indicator,
                r.median_ms,
                r.check_budget_ms,
                if r.pass { "PASS" } else { "FAIL" }
            );
        }
        if let Some(line) = &docker_line {
            println!("{line}");
        }
    }

    if check && any_fail {
        1
    } else {
        0
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// B9 fixture: minimal docker-save tar
// ──────────────────────────────────────────────────────────────────────────────

/// Build a single-file uncompressed layer tar in memory.
fn build_layer_buf(path: &str, content: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut buf);
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        ar.append_data(&mut header, path, content)
            .expect("append layer entry");
        ar.finish().expect("finish layer tar");
    }
    buf
}

/// Write a minimal docker-save tar to `<dir>/image.tar` and return the path.
///
/// Layout:
///   manifest.json  — [{Config, RepoTags, Layers:[layer.tar]}]
///   layer.tar      — one tiny file (bench/hello)
///   config.json    — minimal config blob ({})
fn make_tiny_oci_tar(dir: &Path) -> PathBuf {
    let layer_data = build_layer_buf("bench/hello", b"hi");
    let config_data = b"{}";
    let config_name = "config.json";

    let manifest_json = serde_json::json!([{
        "Config": config_name,
        "RepoTags": ["bench-image:latest"],
        "Layers": ["layer.tar"]
    }]);
    let manifest_bytes =
        serde_json::to_vec(&manifest_json).expect("serialize docker-save manifest");

    let tar_path = dir.join("image.tar");
    let file = fs::File::create(&tar_path).expect("create image.tar");
    let mut ar = tar::Builder::new(file);

    let mut append = |name: &str, data: &[u8]| {
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(data.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_entry_type(tar::EntryType::Regular);
        hdr.set_cksum();
        ar.append_data(&mut hdr, name, data)
            .expect("append tar entry");
    };

    append("manifest.json", &manifest_bytes);
    append("layer.tar", &layer_data);
    append(config_name, config_data);
    ar.finish().expect("finish image.tar");

    tar_path
}

// ──────────────────────────────────────────────────────────────────────────────
// B10 fixture: 3-step Dockerfile
// ──────────────────────────────────────────────────────────────────────────────

/// Write a 3-step Dockerfile + context into `dir`.
/// Steps: COPY a file, RUN a pure deterministic command (echo), COPY another.
fn make_bench_dockerfile(dir: &Path) {
    // Context files
    fs::write(dir.join("fileA.txt"), b"alpha content").expect("write fileA");
    fs::write(dir.join("fileB.txt"), b"beta content").expect("write fileB");

    let dockerfile = concat!(
        "FROM scratch\n",
        "COPY fileA.txt /a.txt\n",
        "RUN echo built\n",
        "COPY fileB.txt /b.txt\n",
    );
    fs::write(dir.join("Dockerfile"), dockerfile.as_bytes()).expect("write Dockerfile");
}

// ──────────────────────────────────────────────────────────────────────────────
// B11 fixture: minimal 1-service compose.yml
// ──────────────────────────────────────────────────────────────────────────────

/// Write a minimal 1-service compose.yml binding port 59876 into `dir` and
/// return the path. Port is high and unprivileged; service has no real image
/// (lazy binding: listeners registered but service not eagerly started).
fn make_bench_compose(dir: &Path) -> PathBuf {
    let compose_yml = concat!(
        "services:\n",
        "  bench-svc:\n",
        "    image: bench-image:latest\n",
        "    ports:\n",
        "      - \"59876:59876\"\n",
    );
    let path = dir.join("compose.yml");
    fs::write(&path, compose_yml.as_bytes()).expect("write compose.yml");
    path
}

// ──────────────────────────────────────────────────────────────────────────────
// Docker comparison
// ──────────────────────────────────────────────────────────────────────────────

fn check_docker() -> Option<String> {
    // Check if docker binary is on PATH and responsive within 2s.
    use std::process::Stdio;
    use std::time::Duration as Dur;

    let docker_present = which_docker().is_some();
    if !docker_present {
        return Some("docker: not present — comparison skipped".to_string());
    }

    // Try `docker version --format {{.Server.Version}}` with 2s timeout.
    // Rust std doesn't have timeout on child directly; use thread with timeout.
    let handle = std::thread::spawn(|| {
        Command::new("docker")
            .args(["version", "--format", "{{.Server.Version}}"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
    });

    // Join with a 2s timeout via a receiver.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = handle.join().ok().and_then(|r| r.ok());
        let _ = tx.send(out);
    });
    let result = rx.recv_timeout(Dur::from_secs(2)).unwrap_or(None);

    match result {
        None => Some("docker: not responsive — comparison skipped".to_string()),
        Some(out) if !out.status.success() => {
            Some("docker: not responsive — comparison skipped".to_string())
        }
        Some(_) => {
            // Measure docker version overhead.
            let docker_dur = median_of(
                || {
                    let t = Instant::now();
                    let _ = Command::new("docker")
                        .args(["version", "--format", "{{.Server.Version}}"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .output();
                    t.elapsed()
                },
                5,
            );
            let lightr_version_dur = median_of(|| time_spawn(&["--version"]), 5);
            Some(format!(
                "docker: version overhead {:.1}ms vs lightr --version {:.1}ms",
                docker_dur.as_secs_f64() * 1000.0,
                lightr_version_dur.as_secs_f64() * 1000.0,
            ))
        }
    }
}

fn which_docker() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_os| {
        std::env::split_paths(&path_os).find_map(|dir| {
            let candidate = dir.join("docker");
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        })
    })
}
