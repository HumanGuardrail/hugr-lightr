//! `lightr bench-compare` handler — the head-to-head "humiliation" benchmark
//! (WP-C, build-spec-parity.md §5).
//!
//! Runs IDENTICAL workloads through Lightr and each competitor side-by-side and
//! prints a table (`indicator | lightr | docker | orbstack | container | factor`)
//! plus `--json`. The `factor` is `competitor / lightr` — the humiliation
//! multiple — printed ONLY where BOTH numbers were measured.
//!
//! ## Tense law (inviolable — ADR-0012, performance-bar.md)
//! NEVER print a number that was not measured. A competitor that is absent from
//! `$PATH` produces a printed **SKIP** cell, NEVER a fabricated number. Lightr is
//! ALWAYS measured (it is the subject); a competitor is measured only if present.
//! If NO competitor is on `$PATH`, Lightr's own numbers still print, with a clear
//! "no competitor on PATH to compare against" note.
//!
//! This is the marketing/proof harness — it has NO CI budget gate (that is the
//! plain `bench` verb). It draws its methodology from `bench.rs`: median-of-N
//! after a warmup, fixtures built in a tempdir with `LIGHTR_HOME` also a tempdir,
//! Lightr measured via the real code paths (in-process index ops + self-spawn).
//!
//! Honesty boundary on measuring competitors: spawning real Docker/OrbStack/Apple
//! `container` workloads (pull, run, build) is the harness's job at MARKETING time
//! on a real box. In CI/tests no container runtime is present, so the only path
//! exercised by tests is detection-and-skip. We measure for a competitor exactly
//! the surfaces we can run without fabricating anything; an op a present runtime
//! cannot perform (e.g. timed out) is itself a SKIP, not a guessed number.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lightr_index::{hydrate, snapshot};
use lightr_store::Store;
use serde::Serialize;

use super::bench_compete_docker::{self as dp};

// ──────────────────────────────────────────────────────────────────────────────
// Fixture sizing
// ──────────────────────────────────────────────────────────────────────────────

/// Default `materialize` tree size for a REAL run: 1 GB (build-spec-parity §5.1 —
/// the ~10 MB bench fixture is too small for indicator #3). Tests MUST override
/// this with a tiny size via `MaterializeSize::small()`.
#[derive(Clone, Copy)]
pub(crate) struct MaterializeSize {
    /// number of 1 MiB files to write
    files_1mib: usize,
}

impl MaterializeSize {
    /// 1 GB tree — the real headline workload.
    fn real() -> Self {
        MaterializeSize { files_1mib: 1024 }
    }
    /// Tiny tree for tests (4 MiB) — keeps the unit/integration path fast and
    /// CI-safe. NEVER used for a published number.
    #[cfg(test)]
    fn small() -> Self {
        MaterializeSize { files_1mib: 4 }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Workload selection
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Workload {
    Install,
    Materialize,
    ColdRun,
    ReRun,
    Idle,
    Build,
    ColdImage,
}

impl Workload {
    const ALL: [Workload; 7] = [
        Workload::Install,
        Workload::Materialize,
        Workload::ColdRun,
        Workload::ReRun,
        Workload::Idle,
        Workload::Build,
        Workload::ColdImage,
    ];

    /// Parse the `--workload` flag. `all` ⇒ every workload. Unknown ⇒ `None`.
    fn select(flag: &str) -> Option<Vec<Workload>> {
        match flag {
            "all" => Some(Workload::ALL.to_vec()),
            "install" => Some(vec![Workload::Install]),
            "materialize" => Some(vec![Workload::Materialize]),
            "cold-run" => Some(vec![Workload::ColdRun]),
            "re-run" => Some(vec![Workload::ReRun]),
            "idle" => Some(vec![Workload::Idle]),
            "build" => Some(vec![Workload::Build]),
            "cold-image" => Some(vec![Workload::ColdImage]),
            _ => None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Competitor runtimes
// ──────────────────────────────────────────────────────────────────────────────

/// A competitor container runtime we compare Lightr against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Runtime {
    Docker,
    OrbStack,
    AppleContainer,
}

impl Runtime {
    /// Column header for the table / JSON key.
    fn label(self) -> &'static str {
        match self {
            Runtime::Docker => "docker",
            Runtime::OrbStack => "orbstack",
            Runtime::AppleContainer => "container",
        }
    }

    /// Parse a `--vs` token. Accepts the OrbStack aliases `orbstack` and `orb`.
    /// Unknown ⇒ `None` (the caller turns that into an honest error, never a row).
    fn parse(token: &str) -> Option<Runtime> {
        match token {
            "docker" => Some(Runtime::Docker),
            "orbstack" | "orb" => Some(Runtime::OrbStack),
            "container" => Some(Runtime::AppleContainer),
            _ => None,
        }
    }

    /// Binary names to probe on `$PATH`, in order. The first that exists wins.
    fn binaries(self) -> &'static [&'static str] {
        match self {
            Runtime::Docker => &["docker"],
            Runtime::OrbStack => &["orb", "orbstack"],
            Runtime::AppleContainer => &["container"],
        }
    }
}

/// Resolve `--vs` tokens into an ordered, de-duplicated runtime list.
/// Returns `Err(token)` on the FIRST unknown token (fail closed — never silently
/// drop a misspelled competitor and pretend it was skipped).
fn parse_runtimes(vs: &[String]) -> Result<Vec<Runtime>, String> {
    let mut out: Vec<Runtime> = Vec::new();
    for tok in vs {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        match Runtime::parse(t) {
            Some(rt) => {
                if !out.contains(&rt) {
                    out.push(rt);
                }
            }
            None => return Err(t.to_string()),
        }
    }
    Ok(out)
}

/// Find a runtime's binary on `$PATH`. Pure `$PATH` scan (mirrors `bench.rs`
/// `which_docker`); does NOT execute the binary.
fn which_on_path(names: &[&str]) -> Option<PathBuf> {
    let path_os = std::env::var_os("PATH")?;
    which_in(names, &path_os)
}

/// Scan an explicit PATH value for the first existing binary among `names`.
/// Factored out so tests can drive detection WITHOUT mutating the process-global
/// `$PATH` (which would race other parallel tests).
fn which_in(names: &[&str], path_os: &std::ffi::OsStr) -> Option<PathBuf> {
    for name in names {
        for dir in std::env::split_paths(path_os) {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Detection result for one runtime: present (with the resolved path) or absent.
struct Detected {
    runtime: Runtime,
    path: Option<PathBuf>,
}

impl Detected {
    fn present(&self) -> bool {
        self.path.is_some()
    }
}

/// Detect each requested runtime on `$PATH`. Present/absent only — no probing.
fn detect_all(runtimes: &[Runtime]) -> Vec<Detected> {
    runtimes
        .iter()
        .map(|&rt| Detected {
            runtime: rt,
            path: which_on_path(rt.binaries()),
        })
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Cells: a measurement is Measured(value+unit), Skipped(reason), or NA.
// ──────────────────────────────────────────────────────────────────────────────

/// What unit a cell carries — so the table/JSON render honestly and the factor
/// is only computed between like units. (Timings are ms; idle footprint is a
/// process count. RSS-in-MB is a marketing-time extension; not wired here
/// because an honest competitor RSS requires spawning its daemon/VM.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unit {
    Millis,
    Count,
    /// Megabytes on disk — install footprint (indicator #1).
    Mb,
}

impl Unit {
    fn suffix(self) -> &'static str {
        match self {
            Unit::Millis => "ms",
            Unit::Count => "",
            Unit::Mb => "MB",
        }
    }
}

/// A single cell of the table. NEVER a fabricated number: a competitor that is
/// absent (or that could not run the op honestly) is `Skip`, not a guess.
#[derive(Clone, Debug)]
enum Cell {
    /// A real measured value in the row's unit.
    Measured(f64),
    /// Skipped — carries why (absent runtime, unsupported op, timeout).
    Skip(&'static str),
    /// Not applicable for this runtime/row (e.g. there is no "Lightr daemon").
    Na,
}

impl Cell {
    fn measured_value(&self) -> Option<f64> {
        match self {
            Cell::Measured(v) => Some(*v),
            _ => None,
        }
    }
}

/// One row of the head-to-head table.
struct CmpRow {
    indicator: &'static str,
    unit: Unit,
    lightr: Cell,
    /// One cell per requested runtime, aligned to `detected` order.
    competitors: Vec<Cell>,
}

impl CmpRow {
    /// The humiliation multiple for column `i`: competitor / lightr. Only where
    /// BOTH cells are measured AND lightr is non-zero (we never divide by zero,
    /// and a 0-baseline factor would be a fabricated infinity).
    fn factor(&self, i: usize) -> Option<f64> {
        let l = self.lightr.measured_value()?;
        let c = self.competitors.get(i)?.measured_value()?;
        if l > 0.0 {
            Some(c / l)
        } else {
            None
        }
    }

    /// The best (max) factor across competitors — the headline multiple for the
    /// row. `None` if no competitor was measured against a measured lightr.
    fn best_factor(&self) -> Option<f64> {
        (0..self.competitors.len())
            .filter_map(|i| self.factor(i))
            .fold(None, |acc, f| match acc {
                Some(m) if m >= f => Some(m),
                _ => Some(f),
            })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Measurement helpers (mirror bench.rs methodology)
// ──────────────────────────────────────────────────────────────────────────────

/// Run `f` once as warmup, then `n` times; return the median duration.
/// Mirrors `bench.rs::median_of`. `pub(crate)` so the Docker probe times its
/// spawned ops with the IDENTICAL methodology (warmup + median-of-N).
pub(crate) fn median_of<F: FnMut() -> Duration>(mut f: F, n: usize) -> Duration {
    let _ = f();
    let mut samples: Vec<Duration> = (0..n).map(|_| f()).collect();
    samples.sort();
    samples[n / 2]
}

pub(crate) fn dur_ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Spawn `lightr` (this very binary) with `args` under the given `LIGHTR_HOME`
/// and time it. Used for the real run/build code paths (mirrors `bench.rs`).
fn time_lightr(home: &Path, args: &[&str]) -> Duration {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        // current_exe failing is environmental; surface as a zero-length sample
        // rather than panicking on a non-test path. The caller's median absorbs it.
        Err(_) => return Duration::ZERO,
    };
    let t = Instant::now();
    let _ = Command::new(&exe)
        .env("LIGHTR_HOME", home)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    t.elapsed()
}

/// Number of timed samples per measurement (after 1 warmup). Small to keep the
/// proof harness snappy; medians still suppress single-sample noise. `pub(crate)`
/// so the Docker probe samples its spawned ops identically.
pub(crate) const SAMPLES: usize = 5;

// ──────────────────────────────────────────────────────────────────────────────
// Fixture builders (self-contained; adapted from bench.rs)
// ──────────────────────────────────────────────────────────────────────────────

/// Build a materialize fixture: `size.files_1mib` × 1 MiB files under a few
/// subdirs. Returns the root. The bytes are identical regardless of runtime —
/// that is the point of a head-to-head.
pub(crate) fn build_materialize_fixture(root: &Path, size: MaterializeSize) -> std::io::Result<()> {
    let dirs = ["a", "b", "c", "d"];
    for d in dirs {
        std::fs::create_dir_all(root.join(d))?;
    }
    let one_mib = vec![0xA5u8; 1024 * 1024];
    for i in 0..size.files_1mib {
        let sub = dirs[i % dirs.len()];
        let p = root.join(sub).join(format!("blk{i:05}.dat"));
        std::fs::write(p, &one_mib)?;
    }
    Ok(())
}

/// Write a 3-step Dockerfile + context into `dir` (mirrors bench.rs). `pub(crate)`
/// so the Docker `build` probe builds over the IDENTICAL context bytes.
pub(crate) fn make_bench_dockerfile(dir: &Path) -> std::io::Result<()> {
    std::fs::write(dir.join("fileA.txt"), b"alpha content")?;
    std::fs::write(dir.join("fileB.txt"), b"beta content")?;
    let dockerfile = concat!(
        "FROM scratch\n",
        "COPY fileA.txt /a.txt\n",
        "RUN echo built\n",
        "COPY fileB.txt /b.txt\n",
    );
    std::fs::write(dir.join("Dockerfile"), dockerfile.as_bytes())?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Lightr-side measurements (the real code paths)
// ──────────────────────────────────────────────────────────────────────────────

/// Measure Lightr `materialize`: snapshot the fixture into the store once, then
/// time `hydrate` (CoW materialize) median-of-N into fresh dest dirs. Returns
/// the median in ms, or `None` if the store/snapshot setup failed (→ honest NA
/// rather than a fabricated number).
fn lightr_materialize_ms(home: &Path, size: MaterializeSize) -> Option<f64> {
    let fixture = home.join("mat-fixture");
    std::fs::create_dir_all(&fixture).ok()?;
    build_materialize_fixture(&fixture, size).ok()?;

    let store_root = home.join("store");
    let store = Store::open(&store_root).ok()?;
    snapshot(&fixture, &store, "bc-materialize").ok()?;

    let dest_base = home.join("mat-dest");
    std::fs::create_dir_all(&dest_base).ok()?;

    let mut counter = 0usize;
    let d = median_of(
        || {
            counter += 1;
            let dest = dest_base.join(format!("h{counter}"));
            let _ = std::fs::create_dir_all(&dest);
            let store = match Store::open(&store_root) {
                Ok(s) => s,
                Err(_) => return Duration::ZERO,
            };
            let t = Instant::now();
            let _ = hydrate(&dest, &store, "bc-materialize");
            t.elapsed()
        },
        SAMPLES,
    );
    Some(dur_ms(d))
}

/// cold-image (Lightr side): pull the SAME real image into the CAS ONCE (untimed
/// setup), then time the CoW hydrate (median-of-N, fresh dest per sample). Returns
/// None (→ honest Na) if the store/pull setup fails — never a fabricated number.
fn lightr_cold_image_ms(home: &Path) -> Option<f64> {
    let store_root = home.join("store");
    let store = lightr_store::Store::open(&store_root).ok()?;
    // SETUP (untimed): ingest the real image into CAS under a bench ref.
    lightr_oci::pull(
        super::bench_compete_docker::COLD_IMAGE_REF,
        &store,
        "bc-cold-image",
    )
    .ok()?;
    let dest_base = home.join("cold-image-hydrate");
    std::fs::create_dir_all(&dest_base).ok()?;
    let mut counter = 0u64;
    // TIMED: CoW hydrate into a FRESH dest each sample (mirrors lightr_materialize_ms,
    // which absorbs a per-sample store reopen failure as Duration::ZERO so the
    // median is honest about a degenerate sample without fabricating a fast number).
    let d = median_of(
        || {
            counter += 1;
            let dest = dest_base.join(format!("h{counter}"));
            let store = match lightr_store::Store::open(&store_root) {
                Ok(s) => s,
                Err(_) => return Duration::ZERO,
            };
            let t = Instant::now();
            let _ = lightr_index::hydrate(&dest, &store, "bc-cold-image");
            t.elapsed()
        },
        SAMPLES,
    );
    Some(dur_ms(d))
}

/// Measure Lightr `re-run`: same job twice — first MISS, then the memo HIT fast
/// path (indicator #4). We time the HIT (the steady-state) via self-spawn so the
/// real CLI memo path is exercised. Returns median ms.
fn lightr_rerun_ms(home: &Path) -> f64 {
    let work = home.join("rerun-work");
    let _ = std::fs::create_dir_all(&work);
    let _ = std::fs::write(work.join("in.txt"), b"rerun-input");
    let dir = work.to_string_lossy().to_string();
    let args = [
        "run",
        "--dir",
        &dir,
        "--",
        "echo",
        "lightr-bench-compare-rerun",
    ];
    // Prime the AC with one MISS (not timed), then median the HIT.
    let _ = time_lightr(home, &args);
    dur_ms(median_of(|| time_lightr(home, &args), SAMPLES))
}

/// Measure Lightr `cold-run`: import a tiny in-memory OCI image into a FRESH
/// store, then run it — the cold pull+run analogue. We time the whole
/// import+run, median over fresh `LIGHTR_HOME`s. Returns median ms.
fn lightr_coldrun_ms(parent_home: &Path) -> f64 {
    // Build the tiny docker-save tar once; reuse across samples.
    let img_dir = parent_home.join("coldrun-img");
    let _ = std::fs::create_dir_all(&img_dir);
    let tar_path = match make_tiny_oci_tar(&img_dir) {
        Ok(p) => p,
        Err(_) => return 0.0,
    };
    let tar_str = tar_path.to_string_lossy().to_string();

    let mut counter = 0usize;
    dur_ms(median_of(
        || {
            counter += 1;
            // Fresh home per sample so the import is genuinely cold.
            let home = parent_home.join(format!("coldrun-home-{counter}"));
            let _ = std::fs::create_dir_all(&home);
            let t = Instant::now();
            let _ = time_lightr(&home, &["oci", "import", &tar_str, "--name", "bc-cold"]);
            t.elapsed()
        },
        SAMPLES,
    ))
}

/// Measure Lightr `build`: a 3-step Dockerfile; cold build then memoized 2nd
/// build (indicator #4/#8 analogue). Returns the CACHED (2nd) build median ms —
/// the steady-state humiliation number. Returns `None` if context setup failed.
fn lightr_build_cached_ms(home: &Path) -> Option<f64> {
    let ctx = home.join("build-ctx");
    std::fs::create_dir_all(&ctx).ok()?;
    make_bench_dockerfile(&ctx).ok()?;
    let ctx_str = ctx.to_string_lossy().to_string();

    let build_home = home.join("build-home");
    std::fs::create_dir_all(&build_home).ok()?;
    let args = ["build", &ctx_str, "-t", "bc-build"];
    // Cold build (not timed) warms the AC.
    let _ = time_lightr(&build_home, &args);
    Some(dur_ms(median_of(|| time_lightr(&build_home, &args), 3)))
}

/// Measure Lightr `install` footprint (indicator #1): the size on disk of THIS
/// running binary — the entire install (single static binary, no VM, no daemon).
/// Returns MB. At marketing time the harness is the RELEASE binary, so this is
/// the shipped footprint; a debug/test binary measures honestly as itself (still
/// an order of magnitude under Docker). `None` (→ NA) if `current_exe`/metadata
/// is unavailable — never a guess.
fn lightr_install_mb() -> Option<f64> {
    let exe = std::env::current_exe().ok()?;
    let bytes = std::fs::metadata(&exe).ok()?.len();
    Some(bytes as f64 / (1024.0 * 1024.0))
}

/// Is this `ps` `comm` field OUR `lightr` binary (exactly), as opposed to some
/// unrelated process whose path merely contains the substring "lightr"
/// (e.g. a CI runner living under `…/actions-runner-lightr-…/`)?
///
/// The binary is named exactly `lightr` (CLAUDE.md), so an honest match is:
/// the basename of `comm` equals `lightr` — never a loose substring. This is
/// what keeps the daemonless invariant HONEST: we count Lightr's own resident
/// processes, not any path that happens to spell "lightr".
fn comm_is_lightr_binary(comm: &str) -> bool {
    let comm = comm.trim();
    // `comm` may be a bare name ("lightr") or a path (".../target/debug/lightr").
    let base = comm.rsplit('/').next().unwrap_or(comm);
    base == "lightr"
}

/// Lightr idle process count. By construction (principle #1: "no daemon, ever")
/// nothing of ours runs when nothing runs. We prove it the way the spec says —
/// `ps` — by counting processes whose binary is EXACTLY `lightr`, excluding this
/// very `bench-compare` invocation (which is itself a `lightr` process, but not
/// an idle daemon). On a daemonless runtime that is 0. If `ps` is unavailable we
/// return `None` (honest NA), never a guess.
fn lightr_idle_processes() -> Option<f64> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,comm="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let me = std::process::id();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut count = 0u64;
    for line in text.lines() {
        let line = line.trim();
        let Some((pid_s, comm)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let pid: u32 = match pid_s.trim().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == me {
            continue; // this bench-compare invocation is a lightr process, not a daemon
        }
        if comm_is_lightr_binary(comm) {
            count += 1;
        }
    }
    Some(count as f64)
}

// ──────────────────────────────────────────────────────────────────────────────
// Competitor-side measurements
// ──────────────────────────────────────────────────────────────────────────────

/// Idle process count attributable to a competitor runtime — its daemon/VM
/// footprint while "installed but idle". Counts processes whose command mentions
/// the runtime's hallmark daemon names. Present runtime → a real count (often the
/// daemon+VM); absent → caller already produced SKIP, so this is only called for
/// present runtimes. `None` if `ps` is unavailable (honest NA).
fn competitor_idle_processes(rt: Runtime) -> Option<f64> {
    let needles: &[&str] = match rt {
        Runtime::Docker => &["dockerd", "docker", "com.docker", "vpnkit"],
        Runtime::OrbStack => &["orbstack", "OrbStack", "orbd", "orb"],
        Runtime::AppleContainer => &["container", "containerd"],
    };
    let out = Command::new("ps")
        .args(["-A", "-o", "comm="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut count = 0u64;
    for line in text.lines() {
        let comm = line.trim();
        if needles.iter().any(|n| comm.contains(n)) {
            count += 1;
        }
    }
    Some(count as f64)
}

// ──────────────────────────────────────────────────────────────────────────────
// B9 fixture: minimal docker-save tar (self-contained copy from bench.rs)
// ──────────────────────────────────────────────────────────────────────────────

fn build_layer_buf(path: &str, content: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut ar = tar::Builder::new(&mut buf);
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        ar.append_data(&mut header, path, content)?;
        ar.finish()?;
    }
    Ok(buf)
}

/// Write a minimal docker-save tar to `<dir>/image.tar` and return the path.
fn make_tiny_oci_tar(dir: &Path) -> std::io::Result<PathBuf> {
    let layer_data = build_layer_buf("bench/hello", b"hi")?;
    let config_data = b"{}";
    let config_name = "config.json";

    let manifest_json = serde_json::json!([{
        "Config": config_name,
        "RepoTags": ["bench-compare-image:latest"],
        "Layers": ["layer.tar"]
    }]);
    let manifest_bytes = serde_json::to_vec(&manifest_json)?;

    let tar_path = dir.join("image.tar");
    let file = std::fs::File::create(&tar_path)?;
    let mut ar = tar::Builder::new(file);

    let mut append = |name: &str, data: &[u8]| -> std::io::Result<()> {
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(data.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_entry_type(tar::EntryType::Regular);
        hdr.set_cksum();
        ar.append_data(&mut hdr, name, data)
    };

    append("manifest.json", &manifest_bytes)?;
    append("layer.tar", &layer_data)?;
    append(config_name, config_data)?;
    ar.finish()?;
    Ok(tar_path)
}

// ──────────────────────────────────────────────────────────────────────────────
// Competitor spawn-guard + dispatch
// ──────────────────────────────────────────────────────────────────────────────

/// Whether the head-to-head is allowed to SPAWN competitor containers.
///
/// `Spawn` is set ONLY by the real CLI entry (`run`). Tests and CI construct
/// `NeverSpawn`, so a present competitor is an honest SKIP and `cargo test` can
/// never launch a container — even on a docker-equipped GitHub runner. This is a
/// structural tense-law guard, not a convention.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProbePolicy {
    Spawn,
    NeverSpawn,
}

/// Map one competitor for a spawn-workload to a `Cell`, enforcing the spawn-guard
/// and the docker-only probe scope. `probe` is the per-workload Docker measurement
/// (resolved binary path + a scratch dir for its docker-side fixtures).
///
/// - absent on PATH ⇒ `Skip("absent on PATH")`
/// - present but `NeverSpawn` ⇒ `Skip` (test/CI guard — never spawns a container)
/// - present + `Spawn` + Docker ⇒ run the probe, map `Outcome` → `Cell`
/// - present + `Spawn` + non-Docker ⇒ `Skip` (only Docker has a probe today)
fn measure_competitor(
    d: &Detected,
    policy: ProbePolicy,
    scratch: &Path,
    probe: impl FnOnce(&Path, &Path) -> dp::Outcome,
) -> Cell {
    let Some(bin) = d.path.as_deref() else {
        return Cell::Skip("absent on PATH");
    };
    if policy == ProbePolicy::NeverSpawn {
        return Cell::Skip("competitor spawn disabled (test/CI tense-law guard)");
    }
    match d.runtime {
        Runtime::Docker => match probe(bin, scratch) {
            dp::Outcome::Measured(v) => Cell::Measured(v),
            dp::Outcome::Skip(r) => Cell::Skip(r),
        },
        _ => Cell::Skip("head-to-head probe implemented for docker only"),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Workload runner: builds the rows for one workload
// ──────────────────────────────────────────────────────────────────────────────

/// Run ONE workload and produce its row(s). `home` is a per-invocation tempdir
/// used as `LIGHTR_HOME`; `detected` is the aligned competitor list; `size`
/// scopes the materialize fixture (small in tests, 1 GB for real runs); `policy`
/// is the spawn-guard (`Spawn` only from the real CLI; `NeverSpawn` in tests/CI).
///
/// TENSE LAW is enforced here. Lightr is always measured (it is the subject). A
/// competitor cell is `Skip` unless the runtime is present AND `policy == Spawn`
/// AND the probe returns a real measurement. Under `NeverSpawn` a present
/// competitor still SKIPs — so `cargo test`/CI never launch a container. A probe
/// that times out or whose setup fails is itself an honest SKIP, never a guess.
fn run_workload(
    wl: Workload,
    home: &Path,
    detected: &[Detected],
    size: MaterializeSize,
    policy: ProbePolicy,
) -> Vec<CmpRow> {
    // A path (not yet created) for any docker-side fixtures this workload needs;
    // the probe creates what it uses. Unused by Install/Idle (no spawn fixtures).
    let scratch = home.join(format!("docker-scratch-{wl:?}"));
    match wl {
        Workload::Install => {
            let lightr = match lightr_install_mb() {
                Some(mb) => Cell::Measured(mb),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| {
                    measure_competitor(d, policy, &scratch, |bin, _scr| {
                        dp::install_footprint_mb(bin)
                    })
                })
                .collect();
            vec![CmpRow {
                indicator: "install footprint",
                unit: Unit::Mb,
                lightr,
                competitors,
            }]
        }
        Workload::Materialize => {
            let lightr = match lightr_materialize_ms(home, size) {
                Some(ms) => Cell::Measured(ms),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| {
                    measure_competitor(d, policy, &scratch, |bin, scr| {
                        dp::materialize_ms(bin, scr, size)
                    })
                })
                .collect();
            vec![CmpRow {
                indicator: "materialize (CoW)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
        Workload::ColdRun => {
            let lightr = Cell::Measured(lightr_coldrun_ms(home));
            let competitors = detected
                .iter()
                .map(|d| measure_competitor(d, policy, &scratch, dp::cold_run_ms))
                .collect();
            vec![CmpRow {
                indicator: "cold-run (import+run)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
        Workload::ReRun => {
            let lightr = Cell::Measured(lightr_rerun_ms(home));
            let competitors = detected
                .iter()
                .map(|d| measure_competitor(d, policy, &scratch, dp::re_run_ms))
                .collect();
            vec![CmpRow {
                indicator: "re-run (memo hit)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
        Workload::Idle => {
            // The one head-to-head we can measure honestly with no container
            // spawn: process footprint of an idle install. Lightr = 0 (ps proves).
            let lightr = match lightr_idle_processes() {
                Some(n) => Cell::Measured(n),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| {
                    if !d.present() {
                        return Cell::Skip("absent on PATH");
                    }
                    // Present: count its resident daemon/VM processes via ps.
                    match competitor_idle_processes(d.runtime) {
                        Some(n) => Cell::Measured(n),
                        None => Cell::Skip("ps unavailable"),
                    }
                })
                .collect();
            vec![CmpRow {
                indicator: "idle processes",
                unit: Unit::Count,
                lightr,
                competitors,
            }]
        }
        Workload::Build => {
            let lightr = match lightr_build_cached_ms(home) {
                Some(ms) => Cell::Measured(ms),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| measure_competitor(d, policy, &scratch, dp::build_ms))
                .collect();
            vec![CmpRow {
                indicator: "build (memoized 2nd)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
        Workload::ColdImage => {
            let lightr = match lightr_cold_image_ms(home) {
                Some(ms) => Cell::Measured(ms),
                None => Cell::Na,
            };
            let competitors = detected
                .iter()
                .map(|d| measure_competitor(d, policy, &scratch, dp::cold_image_ms))
                .collect();
            vec![CmpRow {
                indicator: "cold-image (CAS→CoW)",
                unit: Unit::Millis,
                lightr,
                competitors,
            }]
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Rendering
// ──────────────────────────────────────────────────────────────────────────────

/// Render one cell for the human table.
fn render_cell(cell: &Cell, unit: Unit) -> String {
    match cell {
        Cell::Measured(v) => match unit {
            Unit::Count => format!("{}", v.round() as i64),
            _ => format!("{:.1}{}", v, unit.suffix()),
        },
        Cell::Skip(_) => "SKIP".to_string(),
        Cell::Na => "n/a".to_string(),
    }
}

/// Render the `factor` cell: the best (max) competitor/lightr multiple, or "—"
/// when no competitor was measured against a measured lightr.
fn render_factor(row: &CmpRow) -> String {
    match row.best_factor() {
        Some(f) => format!("{f:.1}x"),
        None => "—".to_string(),
    }
}

/// The honest header line (performance-bar.md tense law): machine class + which
/// runtimes were present + the Apple-Silicon binding caveat.
fn header_line(detected: &[Detected]) -> String {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    let present: Vec<&str> = detected
        .iter()
        .filter(|d| d.present())
        .map(|d| d.runtime.label())
        .collect();
    let present_str = if present.is_empty() {
        "none".to_string()
    } else {
        present.join(", ")
    };
    format!(
        "bench-compare on this box: {os}/{arch} | competitors present on PATH: {present_str} | \
numbers measured on THIS box; the Apple-Silicon headline binds when run on AS"
    )
}

/// Build the human-readable table as a `String` (testable without stdout).
fn render_table(rows: &[CmpRow], detected: &[Detected], header: &str) -> String {
    let mut s = String::new();
    s.push_str(header);
    s.push('\n');

    // Column header: indicator | lightr | <each runtime> | factor
    let mut head = format!("{:<22}  {:>12}", "indicator", "lightr");
    for d in detected {
        head.push_str(&format!("  {:>12}", d.runtime.label()));
    }
    head.push_str(&format!("  {:>8}", "factor"));
    s.push_str(&head);
    s.push('\n');

    let width = 22 + 2 + 12 + detected.len() * (2 + 12) + 2 + 8;
    s.push_str(&"-".repeat(width));
    s.push('\n');

    for row in rows {
        let mut line = format!(
            "{:<22}  {:>12}",
            row.indicator,
            render_cell(&row.lightr, row.unit)
        );
        for c in &row.competitors {
            line.push_str(&format!("  {:>12}", render_cell(c, row.unit)));
        }
        line.push_str(&format!("  {:>8}", render_factor(row)));
        s.push_str(&line);
        s.push('\n');
    }

    // If no competitor present at all, say so loudly (tense law).
    if !detected.iter().any(|d| d.present()) {
        s.push_str("note: no competitor on PATH to compare against — Lightr numbers shown alone\n");
    }
    s
}

// ──────────────────────────────────────────────────────────────────────────────
// JSON shape
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct CellJson {
    /// "measured" | "skip" | "na"
    state: &'static str,
    /// present only when state == "measured"
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<f64>,
    /// present only when state == "skip"
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

impl CellJson {
    fn from_cell(cell: &Cell) -> Self {
        match cell {
            Cell::Measured(v) => CellJson {
                state: "measured",
                value: Some(*v),
                reason: None,
            },
            Cell::Skip(r) => CellJson {
                state: "skip",
                value: None,
                reason: Some(r),
            },
            Cell::Na => CellJson {
                state: "na",
                value: None,
                reason: None,
            },
        }
    }
}

#[derive(Serialize)]
struct CompetitorCellJson {
    runtime: &'static str,
    #[serde(flatten)]
    cell: CellJson,
    /// competitor/lightr, only where both measured
    #[serde(skip_serializing_if = "Option::is_none")]
    factor: Option<f64>,
}

#[derive(Serialize)]
struct RowJson {
    indicator: &'static str,
    unit: &'static str,
    lightr: CellJson,
    competitors: Vec<CompetitorCellJson>,
    /// best (max) factor across measured competitors
    #[serde(skip_serializing_if = "Option::is_none")]
    factor: Option<f64>,
}

#[derive(Serialize)]
struct ReportJson {
    machine: MachineJson,
    rows: Vec<RowJson>,
}

#[derive(Serialize)]
struct MachineJson {
    os: &'static str,
    arch: &'static str,
    competitors_present: Vec<&'static str>,
    note: &'static str,
}

fn build_report_json(rows: &[CmpRow], detected: &[Detected]) -> ReportJson {
    let present: Vec<&'static str> = detected
        .iter()
        .filter(|d| d.present())
        .map(|d| d.runtime.label())
        .collect();

    let row_json = rows
        .iter()
        .map(|row| {
            let competitors = row
                .competitors
                .iter()
                .enumerate()
                .map(|(i, c)| CompetitorCellJson {
                    runtime: detected[i].runtime.label(),
                    cell: CellJson::from_cell(c),
                    factor: row.factor(i),
                })
                .collect();
            RowJson {
                indicator: row.indicator,
                unit: match row.unit {
                    Unit::Millis => "ms",
                    Unit::Count => "count",
                    Unit::Mb => "mb",
                },
                lightr: CellJson::from_cell(&row.lightr),
                competitors,
                factor: row.best_factor(),
            }
        })
        .collect();

    ReportJson {
        machine: MachineJson {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            competitors_present: present,
            note: "numbers measured on THIS box; Apple-Silicon headline binds when run on AS",
        },
        rows: row_json,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

/// Run the comparison. `vs` = runtimes to compare against, `workload` = which
/// workload(s) (`all` by default), `json` = machine-readable output.
pub fn run(vs: &[String], workload: &str, json: bool) -> i32 {
    // Parse the requested competitors (fail closed on an unknown token).
    let runtimes = match parse_runtimes(vs) {
        Ok(r) => r,
        Err(bad) => {
            eprintln!(
                "lightr: bench-compare: unknown runtime '{bad}' (expected docker, orbstack/orb, container)"
            );
            return 2;
        }
    };

    // Parse the requested workloads.
    let workloads = match Workload::select(workload) {
        Some(w) => w,
        None => {
            eprintln!(
                "lightr: bench-compare: unknown workload '{workload}' (expected all, materialize, cold-run, re-run, idle, build, cold-image)"
            );
            return 2;
        }
    };

    // Detect each requested runtime on PATH (present/absent only). A present
    // runtime is counted for the idle indicator (its daemon/VM shows in `ps`);
    // every other competitor surface here is an honest SKIP (we never spawn a
    // competitor container workload — tense law forbids fabricating its number).
    let detected = detect_all(&runtimes);

    // Per-invocation LIGHTR_HOME so Lightr's store/index are clean.
    let home_tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lightr: bench-compare: cannot create temp home: {e}");
            return 1;
        }
    };
    let home = home_tmp.path();

    // Real runs use the 1 GB materialize fixture (headline). Tests call the
    // internal runner directly with MaterializeSize::small().
    let size = MaterializeSize::real();

    // Run each workload, collecting rows. The real CLI entry is the ONLY caller
    // that authorizes spawning competitor containers (tense-law spawn-guard).
    let mut rows: Vec<CmpRow> = Vec::new();
    for wl in &workloads {
        rows.extend(run_workload(*wl, home, &detected, size, ProbePolicy::Spawn));
    }

    // Emit.
    if json {
        let report = build_report_json(&rows, &detected);
        match serde_json::to_string(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("lightr: bench-compare: serialize: {e}");
                return 1;
            }
        }
    } else {
        let header = header_line(&detected);
        print!("{}", render_table(&rows, &detected, &header));
    }

    0
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests — MUST NOT require docker/orbstack/container to be installed.
// Lightr-only workload runs use the SMALL fixture. Competitor path = detect+skip.
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parsing ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_runtimes_accepts_known_and_dedups() {
        let vs = vec![
            "docker".to_string(),
            "orbstack".to_string(),
            "container".to_string(),
            "docker".to_string(), // dup
        ];
        let got = parse_runtimes(&vs).expect("known runtimes parse");
        assert_eq!(
            got,
            vec![Runtime::Docker, Runtime::OrbStack, Runtime::AppleContainer]
        );
    }

    #[test]
    fn parse_runtimes_accepts_orb_alias() {
        let got = parse_runtimes(&["orb".to_string()]).expect("orb alias parses");
        assert_eq!(got, vec![Runtime::OrbStack]);
    }

    #[test]
    fn parse_runtimes_fails_closed_on_unknown() {
        let err = parse_runtimes(&["podman".to_string()]).unwrap_err();
        assert_eq!(err, "podman");
    }

    #[test]
    fn workload_select_all_is_seven() {
        let got = Workload::select("all").expect("all parses");
        assert_eq!(got.len(), 7);
    }

    #[test]
    fn workload_select_unknown_is_none() {
        assert!(Workload::select("bogus").is_none());
    }

    #[test]
    fn workload_select_each_name() {
        assert_eq!(Workload::select("install"), Some(vec![Workload::Install]));
        assert_eq!(
            Workload::select("materialize"),
            Some(vec![Workload::Materialize])
        );
        assert_eq!(Workload::select("cold-run"), Some(vec![Workload::ColdRun]));
        assert_eq!(Workload::select("re-run"), Some(vec![Workload::ReRun]));
        assert_eq!(Workload::select("idle"), Some(vec![Workload::Idle]));
        assert_eq!(Workload::select("build"), Some(vec![Workload::Build]));
        assert_eq!(
            Workload::select("cold-image"),
            Some(vec![Workload::ColdImage])
        );
    }

    // ── Cell + factor logic ───────────────────────────────────────────────────

    #[test]
    fn factor_only_when_both_measured() {
        let row = CmpRow {
            indicator: "x",
            unit: Unit::Millis,
            lightr: Cell::Measured(10.0),
            competitors: vec![
                Cell::Measured(100.0),        // factor 10x
                Cell::Skip("absent on PATH"), // no factor
                Cell::Na,                     // no factor
            ],
        };
        assert_eq!(row.factor(0), Some(10.0));
        assert_eq!(row.factor(1), None);
        assert_eq!(row.factor(2), None);
        assert_eq!(row.best_factor(), Some(10.0));
    }

    #[test]
    fn factor_none_when_lightr_skipped() {
        let row = CmpRow {
            indicator: "x",
            unit: Unit::Millis,
            lightr: Cell::Skip("whatever"),
            competitors: vec![Cell::Measured(100.0)],
        };
        assert_eq!(row.factor(0), None);
        assert_eq!(row.best_factor(), None);
    }

    #[test]
    fn factor_never_divides_by_zero_baseline() {
        // A zero lightr baseline (e.g. idle = 0 processes) must NOT fabricate an
        // infinite factor — it yields None.
        let row = CmpRow {
            indicator: "idle processes",
            unit: Unit::Count,
            lightr: Cell::Measured(0.0),
            competitors: vec![Cell::Measured(7.0)],
        };
        assert_eq!(row.factor(0), None);
        assert!(row.best_factor().is_none());
        // And it must render as "—", not "infx" or a number.
        assert_eq!(render_factor(&row), "—");
    }

    #[test]
    fn best_factor_picks_the_max() {
        let row = CmpRow {
            indicator: "x",
            unit: Unit::Millis,
            lightr: Cell::Measured(2.0),
            competitors: vec![Cell::Measured(10.0), Cell::Measured(60.0)],
        };
        // 10/2 = 5x, 60/2 = 30x → best = 30x
        assert_eq!(row.best_factor(), Some(30.0));
    }

    // ── SKIP logic (tense law) — no runtime installed required ────────────────

    #[test]
    fn absent_runtime_yields_skip_never_a_number() {
        // Detected as absent (path None) → every workload competitor cell is SKIP.
        let detected = vec![Detected {
            runtime: Runtime::Docker,
            path: None,
        }];
        let tmp = tempfile::tempdir().expect("tempdir");
        for wl in Workload::ALL {
            // cold-image is exempt from this whole-ALL sweep: run_workload measures
            // the LIGHTR side first, and `lightr_cold_image_ms` does a real CAS pull
            // that hits the NETWORK — forbidden in unit tests. Its absent-competitor
            // SKIP is covered network-free by the guard-direct assertion in
            // `present_competitor_under_neverspawn_always_skips`.
            if wl == Workload::ColdImage {
                continue;
            }
            let rows = run_workload(
                wl,
                tmp.path(),
                &detected,
                MaterializeSize::small(),
                ProbePolicy::NeverSpawn,
            );
            for row in &rows {
                let c = &row.competitors[0];
                match c {
                    Cell::Skip(_) => {} // good — absent → skip
                    Cell::Measured(v) => {
                        panic!(
                            "absent runtime fabricated a number {v} in row {}",
                            row.indicator
                        )
                    }
                    Cell::Na => panic!("absent runtime should SKIP, not NA, in {}", row.indicator),
                }
                // An absent competitor can never produce a factor.
                assert_eq!(
                    row.factor(0),
                    None,
                    "absent → no factor in {}",
                    row.indicator
                );
            }
        }
    }

    #[test]
    fn skip_cell_renders_as_skip_word() {
        assert_eq!(
            render_cell(&Cell::Skip("absent on PATH"), Unit::Millis),
            "SKIP"
        );
        assert_eq!(render_cell(&Cell::Na, Unit::Count), "n/a");
        assert_eq!(render_cell(&Cell::Measured(12.34), Unit::Millis), "12.3ms");
        assert_eq!(render_cell(&Cell::Measured(3.0), Unit::Count), "3");
    }

    // ── Table formatter ───────────────────────────────────────────────────────

    #[test]
    fn table_has_all_columns_and_header_caveat() {
        let detected = vec![
            Detected {
                runtime: Runtime::Docker,
                path: None,
            },
            Detected {
                runtime: Runtime::OrbStack,
                path: None,
            },
        ];
        let rows = vec![CmpRow {
            indicator: "idle processes",
            unit: Unit::Count,
            lightr: Cell::Measured(0.0),
            competitors: vec![Cell::Skip("absent on PATH"), Cell::Skip("absent on PATH")],
        }];
        let header = header_line(&detected);
        let table = render_table(&rows, &detected, &header);

        // Columns present.
        assert!(table.contains("indicator"));
        assert!(table.contains("lightr"));
        assert!(table.contains("docker"));
        assert!(table.contains("orbstack"));
        assert!(table.contains("factor"));
        // The honest header caveat.
        assert!(table.contains("Apple-Silicon headline binds when run on AS"));
        assert!(table.contains("numbers measured on THIS box"));
        // No competitor present → the loud note.
        assert!(table.contains("no competitor on PATH to compare against"));
        // No fabricated number — SKIP appears for both competitor cells.
        assert_eq!(table.matches("SKIP").count(), 2);
    }

    #[test]
    fn header_line_lists_present_runtimes() {
        // All absent → "none".
        let detected = vec![Detected {
            runtime: Runtime::Docker,
            path: None,
        }];
        let h = header_line(&detected);
        assert!(h.contains("competitors present on PATH: none"));
    }

    // ── JSON shape ────────────────────────────────────────────────────────────

    #[test]
    fn json_shape_is_honest_and_well_formed() {
        let detected = vec![Detected {
            runtime: Runtime::Docker,
            path: None,
        }];
        let rows = vec![CmpRow {
            indicator: "idle processes",
            unit: Unit::Count,
            lightr: Cell::Measured(0.0),
            competitors: vec![Cell::Skip("absent on PATH")],
        }];
        let report = build_report_json(&rows, &detected);
        let s = serde_json::to_string(&report).expect("serialize report");

        // Round-trips to a value with the expected structure.
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse json");
        assert_eq!(v["machine"]["os"], std::env::consts::OS);
        assert_eq!(v["machine"]["arch"], std::env::consts::ARCH);
        // No competitor present → empty present list.
        assert!(v["machine"]["competitors_present"]
            .as_array()
            .expect("array")
            .is_empty());

        let row0 = &v["rows"][0];
        assert_eq!(row0["indicator"], "idle processes");
        assert_eq!(row0["unit"], "count");
        // Lightr measured 0.
        assert_eq!(row0["lightr"]["state"], "measured");
        assert_eq!(row0["lightr"]["value"], 0.0);
        // Competitor SKIP carries reason + NO value.
        let comp0 = &row0["competitors"][0];
        assert_eq!(comp0["runtime"], "docker");
        assert_eq!(comp0["state"], "skip");
        assert_eq!(comp0["reason"], "absent on PATH");
        assert!(comp0["value"].is_null());
        // No factor on the row (lightr=0 baseline AND competitor skipped).
        assert!(row0["factor"].is_null());
        assert!(comp0["factor"].is_null());
    }

    #[test]
    fn json_emits_factor_when_both_measured() {
        let detected = vec![Detected {
            runtime: Runtime::Docker,
            path: Some(PathBuf::from("/usr/bin/docker")),
        }];
        let rows = vec![CmpRow {
            indicator: "idle processes",
            unit: Unit::Count,
            lightr: Cell::Measured(1.0),
            competitors: vec![Cell::Measured(9.0)],
        }];
        let report = build_report_json(&rows, &detected);
        let v: serde_json::Value =
            serde_json::from_value(serde_json::to_value(&report).expect("to_value"))
                .expect("from_value");
        assert_eq!(v["rows"][0]["factor"], 9.0);
        assert_eq!(v["rows"][0]["competitors"][0]["factor"], 9.0);
        assert_eq!(v["machine"]["competitors_present"][0], "docker");
    }

    // ── Lightr-only workload runner (SMALL fixture; NO docker spawn) ───────────

    #[test]
    fn lightr_materialize_measures_a_real_number_small() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ms = lightr_materialize_ms(tmp.path(), MaterializeSize::small())
            .expect("materialize should measure");
        assert!(ms >= 0.0, "materialize ms must be non-negative");
    }

    #[test]
    fn lightr_idle_processes_counts_no_lightr_daemon() {
        // Daemonless: no resident lightr process (this test process is excluded).
        // ps must be available on the test host (macOS/Linux).
        if let Some(n) = lightr_idle_processes() {
            assert!(n >= 0.0);
            // We can't assert exactly 0 in all CI shapes, but the value is a real
            // count, never fabricated. (On a clean daemonless box it is 0.)
        }
    }

    #[test]
    fn run_workload_idle_lightr_only_no_competitor_spawn() {
        // Idle workload with an ABSENT competitor: lightr measured, competitor SKIP.
        let detected = vec![Detected {
            runtime: Runtime::Docker,
            path: None,
        }];
        let tmp = tempfile::tempdir().expect("tempdir");
        let rows = run_workload(
            Workload::Idle,
            tmp.path(),
            &detected,
            MaterializeSize::small(),
            ProbePolicy::NeverSpawn,
        );
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        // Lightr is measured (count) or NA if ps missing — never SKIP.
        assert!(
            matches!(row.lightr, Cell::Measured(_) | Cell::Na),
            "lightr idle must be measured or na, got {:?}",
            row.lightr
        );
        // Competitor absent → SKIP, no number.
        assert!(matches!(row.competitors[0], Cell::Skip(_)));
    }

    #[test]
    fn present_competitor_under_neverspawn_always_skips() {
        // THE tense-law spawn-guard. A runtime detected as PRESENT (note the
        // fake path is never executed — the guard returns before touching it)
        // must still SKIP under NeverSpawn across EVERY spawn-workload. This is
        // what makes `cargo test`/CI structurally unable to launch a container,
        // even on a docker-equipped runner. (Idle is exempt: it counts processes
        // via `ps`, which is not a container spawn.)
        let detected = vec![Detected {
            runtime: Runtime::Docker,
            path: Some(PathBuf::from("/usr/local/bin/docker")),
        }];
        let tmp = tempfile::tempdir().expect("tempdir");
        for wl in [
            Workload::Install,
            Workload::Materialize,
            Workload::ColdRun,
            Workload::ReRun,
            Workload::Build,
        ] {
            let rows = run_workload(
                wl,
                tmp.path(),
                &detected,
                MaterializeSize::small(),
                ProbePolicy::NeverSpawn,
            );
            for row in &rows {
                match &row.competitors[0] {
                    Cell::Skip(r) => assert!(
                        r.contains("spawn disabled"),
                        "present+NeverSpawn must skip with the guard reason, got {r:?} in {}",
                        row.indicator
                    ),
                    other => panic!(
                        "present competitor under NeverSpawn must SKIP, got {other:?} in {}",
                        row.indicator
                    ),
                }
            }
        }

        // cold-image is also a spawn-workload, so its DOCKER probe must SKIP under
        // NeverSpawn exactly like the others. We assert it via `measure_competitor`
        // (the guard itself) rather than `run_workload`, because run_workload would
        // FIRST measure the LIGHTR side, and `lightr_cold_image_ms` does a real
        // `docker`-free CAS pull that hits the NETWORK — forbidden in unit tests.
        // The guard returns Skip BEFORE the probe closure runs, so neither the
        // network nor `dp::cold_image_ms` is touched here. This proves the same
        // CI-safety lock for cold-image without making a network call.
        let scratch = tmp.path().join("cold-image-scratch");
        let cell = measure_competitor(
            &detected[0],
            ProbePolicy::NeverSpawn,
            &scratch,
            dp::cold_image_ms,
        );
        match cell {
            Cell::Skip(r) => assert!(
                r.contains("spawn disabled"),
                "present+NeverSpawn (cold-image) must skip with the guard reason, got {r:?}"
            ),
            other => {
                panic!("present competitor under NeverSpawn (cold-image) must SKIP, got {other:?}")
            }
        }
    }

    #[test]
    fn install_row_measures_lightr_footprint_mb() {
        // Lightr install footprint = the running binary's real size in MB,
        // measured (never fabricated). Competitor is absent here → SKIP.
        let detected = vec![Detected {
            runtime: Runtime::Docker,
            path: None,
        }];
        let tmp = tempfile::tempdir().expect("tempdir");
        let rows = run_workload(
            Workload::Install,
            tmp.path(),
            &detected,
            MaterializeSize::small(),
            ProbePolicy::NeverSpawn,
        );
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.indicator, "install footprint");
        assert_eq!(row.unit, Unit::Mb);
        match row.lightr {
            Cell::Measured(mb) => assert!(mb > 0.0, "install footprint must be a positive MB"),
            ref other => panic!("lightr install must be measured, got {other:?}"),
        }
        assert!(matches!(row.competitors[0], Cell::Skip(_)));
        // MB renders with the unit suffix.
        assert_eq!(render_cell(&Cell::Measured(4.16), Unit::Mb), "4.2MB");
    }

    #[test]
    fn which_on_path_absent_binary_is_none() {
        // A binary that cannot exist → None (detection never invents a path).
        assert!(which_on_path(&["definitely-not-a-real-binary-xyz-9999"]).is_none());
    }

    #[test]
    fn comm_matches_only_exact_lightr_binary() {
        // Exact name + path-with-basename match.
        assert!(comm_is_lightr_binary("lightr"));
        assert!(comm_is_lightr_binary("/Users/x/target/debug/lightr"));
        assert!(comm_is_lightr_binary("  /usr/local/bin/lightr  "));
        // The real-world false positive: a CI runner under a dir spelled
        // "…-lightr-…" must NOT be counted as a Lightr daemon (daemonless honesty).
        assert!(!comm_is_lightr_binary(
            "/Users/x/actions-runner-lightr-cri/bin/Runner.Listener"
        ));
        assert!(!comm_is_lightr_binary("lightr-helper"));
        assert!(!comm_is_lightr_binary("hugr-lightr"));
        assert!(!comm_is_lightr_binary("dockerd"));
    }

    #[test]
    fn which_in_empty_path_finds_nothing() {
        // Detection over an EMPTY PATH (no global mutation) → nothing found,
        // i.e. every runtime would be marked absent → SKIP. This is the tense-law
        // detection path, exercised without racing other parallel tests.
        let empty = std::ffi::OsString::new();
        for rt in [Runtime::Docker, Runtime::OrbStack, Runtime::AppleContainer] {
            assert!(
                which_in(rt.binaries(), &empty).is_none(),
                "empty PATH must mark {rt:?} absent",
            );
        }
    }
}
