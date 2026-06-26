//! Shared on-disk state + helpers for the LightrBackend container/exec/image
//! planes (WP-CRI-MVP).
//!
//! PROVENANCE: the persisted record shapes, the atomic-write law, the id
//! scheme, the CRI-log line format, and the filter predicates are TRANSCRIBED
//! from the conformance reference — `lightr-cri/crates/lightr-cri-fake/src/
//! lib.rs` (the fake that passes critest). The real engine (lightr-run /
//! lightr-oci / lightr-store) supplies execution + the image plane; the fake
//! supplies the CRI-shaped state model and the kubelet log format. Drift from
//! the seam is caught later by the shared conformance vectors (WP-CRI-VECTORS).
//!
//! Crash-only law (ADR-0017): every record is written tmp+fsync+rename BEFORE
//! the mutating call returns; the in-memory cache is a view rebuilt from disk
//! on `open`. The state root is INJECTED (`LightrBackend::home`), never read
//! from process-global env — keeps instances independent and tests parallel.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use crate::vocab::{
    BackendError, ContainerConfig, ContainerFilter, ContainerId, ContainerState, ContainerStatus,
    Result, SandboxId,
};

// ── Persisted records (extend the seam status with the backend's run handle) ──

/// On-disk container record. Mirrors `ContainerStatus` plus the fields the
/// backend owns to drive the process: the spawned `pid` and the engine
/// `run_id` (the lightr-run run-dir id, when the start path uses the engine).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ContainerRecord {
    pub id: ContainerId,
    pub sandbox: SandboxId,
    pub config: ContainerConfig,
    pub state: ContainerState,
    pub created_at_nanos: i64,
    pub started_at_nanos: i64,
    pub finished_at_nanos: i64,
    pub exit_code: i32,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub message: String,
    /// PID of the spawned process; 0 = not started / exited.
    #[serde(default)]
    pub pid: u32,
    /// WP-#99: which execution path backs this container. `"ns"` ⇒ the ns engine
    /// (real image rootfs + pod netns); empty ⇒ today's host-process fallback.
    /// Drives `stop`: ns ⇒ `cgroup.kill` (kills the in-pidns PID 1 + all
    /// descendants), host ⇒ `kill(rec.pid)`. `serde(default)` ⇒ old records load.
    #[serde(default)]
    pub engine: String,
    /// WP-#99: the cgroup-v2 leaf this ns container lives in (`lightr-cri-<cid>`);
    /// empty for the host path. `stop` writes `<root>/<cgroup_name>/cgroup.kill`.
    #[serde(default)]
    pub cgroup_name: String,
}

// ── Time + id ────────────────────────────────────────────────────────────────

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

/// Unique id `<prefix><nanos>-<counter>` (transcribed from the fake). The
/// per-process atomic counter disambiguates ids minted within the same nanos.
pub fn new_id(prefix: &str) -> String {
    let n = now_nanos();
    let c = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{prefix}{n}-{c}")
}

// ── Atomic write (tmp + fsync + rename) ──────────────────────────────────────

fn atomic_write(dir: &Path, filename: &str, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp_path = dir.join(format!(".tmp-{pid}-{nanos}"));
    let final_path = dir.join(filename);
    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

pub fn atomic_write_json<T: serde::Serialize>(dir: &Path, filename: &str, value: &T) -> Result<()> {
    let data =
        serde_json::to_vec(value).map_err(|e| BackendError::Internal(format!("serialize: {e}")))?;
    atomic_write(dir, filename, &data).map_err(BackendError::Io)
}

// ── pid liveness ─────────────────────────────────────────────────────────────

/// True iff `pid` is a live process. Unix: `kill(pid, 0) == 0`. On non-unix
/// there is no portable cheap probe, so report not-alive (probe-truthful: the
/// container plane is Linux-real; the windows gate only compiles this crate).
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}

// ── Status normalization (critest reason law) ────────────────────────────────

/// Build the seam `ContainerStatus` from a record, applying the critest
/// normalization the fake's `rec_to_status` applies: a terminal container's
/// `reason` is exactly `Completed` (exit 0) or `Error` (non-zero, incl. signal
/// kill), with the raw human detail preserved in `message`; and the
/// `config.log_path` is rewritten to the ABSOLUTE path (sandbox `log_directory`
/// joined with the relative `log_path`) so `crictl logs` can lstat it.
pub fn rec_to_status(rec: &ContainerRecord, sandbox_log_dir: &str) -> ContainerStatus {
    let (reason, message) = if rec.state == ContainerState::Exited {
        let normalized = if rec.exit_code == 0 {
            "Completed"
        } else {
            "Error"
        };
        let message = if rec.message.is_empty() {
            rec.reason.clone()
        } else {
            rec.message.clone()
        };
        (normalized.to_string(), message)
    } else {
        (rec.reason.clone(), rec.message.clone())
    };
    let mut config = rec.config.clone();
    if !config.log_path.is_empty() && !sandbox_log_dir.is_empty() {
        config.log_path = Path::new(sandbox_log_dir)
            .join(&config.log_path)
            .to_string_lossy()
            .into_owned();
    }
    ContainerStatus {
        id: rec.id.clone(),
        sandbox: rec.sandbox.clone(),
        config,
        state: rec.state,
        created_at_nanos: rec.created_at_nanos,
        started_at_nanos: rec.started_at_nanos,
        finished_at_nanos: rec.finished_at_nanos,
        exit_code: rec.exit_code,
        reason,
        message,
    }
}

// ── Filters ──────────────────────────────────────────────────────────────────

pub fn container_matches(rec: &ContainerRecord, filter: &ContainerFilter) -> bool {
    if let Some(id) = &filter.id {
        if &rec.id != id {
            return false;
        }
    }
    if let Some(sb) = &filter.sandbox {
        if &rec.sandbox != sb {
            return false;
        }
    }
    if let Some(state) = &filter.state {
        if &rec.state != state {
            return false;
        }
    }
    for (k, v) in &filter.label_selector {
        if rec.config.labels.get(k).map(String::as_str) != Some(v.as_str()) {
            return false;
        }
    }
    true
}

// ── CRI log line format (§C: <RFC3339Nano> <stream> <F|P> <data>) ────────────

/// Format one CRI log record. `F` = full line (data ends in `\n`); `P` =
/// partial. Transcribed byte-for-byte from the fake so the kubelet log parser
/// (and `crictl logs`) reads identical framing.
pub fn cri_log_line(stream: &str, data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let (y, mo, d, h, mi, s) = epoch_to_ymd_hms(now.as_secs());
    let ts = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        y,
        mo,
        d,
        h,
        mi,
        s,
        now.subsec_nanos()
    );
    let tag = if data.ends_with(b"\n") { "F" } else { "P" };
    let mut out = Vec::with_capacity(ts.len() + 4 + stream.len() + data.len() + 1);
    write!(out, "{ts} {stream} {tag} ").unwrap();
    out.extend_from_slice(data);
    if !data.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}

/// Minimal UTC decomposition from a Unix epoch second (no external dep).
/// Transcribed from the fake (Rata-Die variant shifted to 1 March 2000).
fn epoch_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y as u32, mo as u32, d as u32, h as u32, m as u32, s as u32)
}

/// Open (create-or-append) the CRI log file at `log_dir/log_path`, creating
/// parent dirs. `None` when either component is empty (no log requested).
/// The empty file must exist from container start (kubelet law §C).
pub fn open_cri_log(log_dir: &str, log_path: &str) -> std::io::Result<Option<fs::File>> {
    if log_dir.is_empty() || log_path.is_empty() {
        return Ok(None);
    }
    let path = Path::new(log_dir).join(log_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    Ok(Some(f))
}

// ── exit-code mapping (signal → 128+sig) ─────────────────────────────────────

/// Map an exited child status to `(exit_code, reason)`: signal kill →
/// `(128+sig, "killed-by-signal-N")`, normal exit → `(code, "")`. Used by the
/// container reaper to record the terminal record.
pub fn signal_or_code(s: &std::process::ExitStatus) -> (i32, String) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return (128 + sig, format!("killed-by-signal-{sig}"));
        }
    }
    (s.code().unwrap_or(0), String::new())
}

/// Tee thread: the SINGLE reader of one container stream. Reads raw chunks and
/// writes one CRI-formatted record per `\n`-terminated line (F tag); a trailing
/// partial line is flushed as a P record at EOF. Transcribed from the fake's
/// log path. unix uses the fan-out tee in `stream` (which also feeds attachers —
/// WP-CRI-STREAM); this log-only variant is the non-unix fallback.
#[cfg(not(unix))]
pub fn spawn_tee_thread(
    stream: &'static str,
    reader: impl std::io::Read + Send + 'static,
    log: std::sync::Arc<std::sync::Mutex<Option<fs::File>>>,
) {
    std::thread::spawn(move || {
        use std::io::Write;
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            pending.extend_from_slice(&buf[..n]);
            while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                let formatted = cri_log_line(stream, &line);
                if let Some(f) = log.lock().unwrap().as_mut() {
                    let _ = f.write_all(&formatted);
                }
            }
        }
        if !pending.is_empty() {
            let formatted = cri_log_line(stream, &pending);
            if let Some(f) = log.lock().unwrap().as_mut() {
                let _ = f.write_all(&formatted);
            }
        }
    });
}

pub fn exit_code_from_status(status: &std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    status.code().unwrap_or(0)
}

// ── BackendError <- LightrError ──────────────────────────────────────────────

/// Map a hugr-lightr engine error onto the seam's `BackendError`, preserving
/// the kind (NotFound for missing refs, InvalidArgument for bad refs) so the
/// CRI shell can render faithful gRPC status codes.
pub fn map_lightr_err(e: lightr_core::LightrError) -> BackendError {
    use lightr_core::LightrError as L;
    match e {
        L::RefNotFound(n) => BackendError::NotFound(format!("ref {n}")),
        L::NotFound(d) => BackendError::NotFound(format!("object {}", d.to_hex())),
        L::InvalidRef(n) => BackendError::InvalidArgument(format!("invalid ref: {n}")),
        L::InvalidManifest(m) => BackendError::Internal(format!("invalid manifest: {m}")),
        L::Registry { status, msg } => {
            BackendError::Internal(format!("registry error (HTTP {status}): {msg}"))
        }
        L::Integrity { expected, actual } => BackendError::Internal(format!(
            "integrity: expected {} got {}",
            expected.to_hex(),
            actual.to_hex()
        )),
        L::TooLarge { size, cap } => {
            BackendError::InvalidArgument(format!("blob {size} bytes exceeds cap {cap}"))
        }
        L::Io(io) => BackendError::Io(io),
    }
}
