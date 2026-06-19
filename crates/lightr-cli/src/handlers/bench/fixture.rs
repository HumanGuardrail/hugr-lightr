//! Fixture builders and low-level measurement helpers for `lightr bench`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tar;

// ──────────────────────────────────────────────────────────────────────────────
// Fixture
// ──────────────────────────────────────────────────────────────────────────────

pub(super) fn build_fixture(root: &Path) -> std::io::Result<()> {
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
pub(super) fn median_of<F: FnMut() -> Duration>(mut f: F, n: usize) -> Duration {
    // warmup
    let _ = f();
    let mut samples: Vec<Duration> = (0..n).map(|_| f()).collect();
    samples.sort();
    samples[n / 2]
}

pub(super) fn time_spawn(args: &[&str]) -> Duration {
    let exe = std::env::current_exe().expect("current_exe");
    let t = Instant::now();
    let _out = Command::new(&exe).args(args).output().expect("spawn self");
    t.elapsed()
}

// ──────────────────────────────────────────────────────────────────────────────
// B9 fixture: minimal docker-save tar
// ──────────────────────────────────────────────────────────────────────────────

/// Build a single-file uncompressed layer tar in memory.
pub(super) fn build_layer_buf(path: &str, content: &[u8]) -> Vec<u8> {
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
pub(super) fn make_tiny_oci_tar(dir: &Path) -> PathBuf {
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
pub(super) fn make_bench_dockerfile(dir: &Path) {
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
pub(super) fn make_bench_compose(dir: &Path) -> PathBuf {
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

pub(super) fn check_docker() -> Option<String> {
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

pub(super) fn which_docker() -> Option<PathBuf> {
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
