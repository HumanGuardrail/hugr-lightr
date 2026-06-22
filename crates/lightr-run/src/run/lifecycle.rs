//! Run-instance lifecycle PRIMITIVES (SKELETON-FREEZE) — the daemonless
//! building blocks the container-verb WPs (LIFE-02..20: rm / kill / start /
//! restart / wait / stop) call so each verb's CLI handler reuses one
//! implementation instead of re-deriving run-dir / registry logic and colliding
//! on the shared files.
//!
//! These are PRIMITIVES, not verbs: they carry NO CLI output, NO Docker-faithful
//! flag parsing, NO multi-arg fan-out. The verb behaviour (output format,
//! exit-code mapping, `force`/`time` flag grammar) is the LIFE-02..20 WPs' job;
//! here we only expose pure-ish functions over a single resolved run id.
//!
//! House conventions honoured:
//!   * the `home` root is INJECTED by the caller (like `registry` / `ps`), never
//!     read from the global env here — so the unit tests pass a private tempdir
//!     and run safely in parallel (CI is `cargo test --workspace`, multi-thread).
//!   * resolution (`registry::resolve`) happens at the CALL SITE; these take an
//!     already-resolved id, so a verb resolves once and reuses the id.
//!   * every existing helper is REUSED (registry / ps-logic / stop / ctl /
//!     spawn) — nothing is reinvented.
//!
//! Fail-closed throughout: an unknown / malformed run surfaces an honest error,
//! a RUNNING run is refused for destructive ops unless `force`.

use std::path::{Path, PathBuf};

use lightr_core::{LightrError, Result};

use super::ctl::{ctl_sock_path, send_ctl_op};
use super::paths::{
    parse_exit_code_from_status, pid_alive, read_pid_file, read_spec_on_disk, read_status_file,
};

/// The on-disk directory for a run id: `<home>/run/<id>`. Mirrors
/// `paths::run_dir_for_id` but takes the INJECTED `home` (paths' variant reads
/// the global env, which tests must not depend on).
fn run_dir(home: &Path, id: &str) -> PathBuf {
    home.join("run").join(id)
}

/// Liveness/exit verdict for a single run — the running/exited(code) split
/// `run_status` returns, reusing the exact detection `ps` performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    /// The supervisor's control endpoint is live and its child pid is alive.
    Running,
    /// The supervisor wrote a final `exited <code>` to the status file.
    Exited(i32),
    /// Neither running nor a parseable exit code yet — a freshly-created dir,
    /// or a supervisor that vanished without writing a final status. Fail-closed:
    /// reported honestly rather than guessed as either state.
    Unknown,
}

/// Is this run currently running? Byte-for-byte the liveness test `ps` uses:
/// the ctl endpoint must exist AND the recorded child pid must still be alive.
fn is_running(dir: &Path) -> bool {
    let sock = ctl_sock_path(dir);
    if !sock.exists() {
        return false;
    }
    match read_pid_file(dir) {
        Some(pid) => {
            #[cfg(any(unix, windows))]
            {
                pid_alive(pid)
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = pid;
                true
            }
        }
        None => false,
    }
}

/// running/exited(code) status of a run (reuses `ps`'s detection over a single
/// id instead of listing every run). `Unknown` when the dir has no live endpoint
/// and no parseable `exited <code>` status yet.
pub fn run_status(home: &Path, id: &str) -> Result<RunStatus> {
    let dir = run_dir(home, id);
    if !dir.exists() {
        return Err(LightrError::RefNotFound(id.to_string()));
    }
    if is_running(&dir) {
        return Ok(RunStatus::Running);
    }
    match read_status_file(&dir)
        .as_deref()
        .and_then(parse_exit_code_from_status)
    {
        Some(code) => Ok(RunStatus::Exited(code)),
        None => Ok(RunStatus::Unknown),
    }
}

/// Remove a stopped run's dir AND release its registry name. A RUNNING run is
/// refused (honest error) unless `force` — in which case it is killed first
/// (reusing `stop::stop`), then removed. Idempotent on the name release (absent
/// name is not an error, per `registry::release`).
///
/// Docker parity: `docker rm` refuses a running container unless `-f`, and `-f`
/// kills then removes. Name auto-removal mirrors Docker freeing the name on rm.
pub fn remove_run(home: &Path, id: &str, force: bool) -> Result<()> {
    let dir = run_dir(home, id);
    if !dir.exists() {
        return Err(LightrError::RefNotFound(id.to_string()));
    }

    if is_running(&dir) {
        if !force {
            return Err(LightrError::InvalidRef(format!(
                "cannot remove running run {id}: stop it first or use force"
            )));
        }
        // force: kill first (reuse stop's SIGTERM→grace→SIGKILL ladder), then
        // fall through to remove. A short grace keeps `rm -f` snappy.
        let _ = super::stop::stop(&dir, 1);
    }

    // Release the run's registry name BEFORE removing the dir, so a name freed
    // for re-use even if the dir removal partially fails. The name lives in
    // spec.json; absent/None is a no-op.
    if let Ok(spec) = read_spec_on_disk(&dir) {
        if let Some(name) = spec.name.as_deref() {
            super::registry::release(home, name)?;
        }
    }

    std::fs::remove_dir_all(&dir).map_err(LightrError::Io)?;
    Ok(())
}

/// Re-spawn a STOPPED run in its SAME dir/id from its persisted `SpecOnDisk`
/// (reuses the supervisor launch extracted from `spawn`). Refuses a run that is
/// already running (honest error — Docker's `start` on a running container is a
/// no-op/error; we fail-closed so a verb can map it). The supervisor re-reads
/// spec.json from the dir, so no spec is reconstructed here.
pub fn respawn_run(home: &Path, id: &str) -> Result<()> {
    let dir = run_dir(home, id);
    if !dir.exists() {
        return Err(LightrError::RefNotFound(id.to_string()));
    }
    if is_running(&dir) {
        return Err(LightrError::InvalidRef(format!(
            "run {id} is already running"
        )));
    }
    // The spec must be readable for the supervisor to act on it — fail closed
    // here with a clear error rather than spawning a supervisor that will die.
    read_spec_on_disk(&dir)?;

    // Clear any stale terminal status so a watcher (`wait_run`) does not read the
    // PREVIOUS run's exit code before the re-launched supervisor overwrites it.
    let _ = std::fs::remove_file(dir.join("status"));

    // WP-RC-RESTART: a deliberate `start` re-arms the run — clear any stop marker
    // left by a prior `stop`/`rm -f`, else the re-launched supervisor's re-spawn
    // loop would see the stale marker and refuse to restart the new child.
    let _ = std::fs::remove_file(super::respawn::stop_marker_path(&dir));

    super::spawn::launch_supervisor(&dir)
}

/// Send `signal` to a running run — via the live ctl.sock if present (reusing
/// `ctl::send_ctl_op`, the same `{"op":"signal","sig":N}` wire the supervisor
/// serves), else direct to the recorded pid. Refuses a non-running run (honest
/// error — there is nothing to signal). `signal` is the raw signal number
/// (Docker's `kill -s` maps names→numbers at the CLI layer, a verb's job).
pub fn signal_run(home: &Path, id: &str, signal: i32) -> Result<()> {
    let dir = run_dir(home, id);
    if !dir.exists() {
        return Err(LightrError::RefNotFound(id.to_string()));
    }
    if !is_running(&dir) {
        return Err(LightrError::InvalidRef(format!("run {id} is not running")));
    }

    let sock = ctl_sock_path(&dir);
    if sock.exists() {
        // Reuse the supervisor's signal op (it relays the signal to the child).
        let op = format!(r#"{{"op":"signal","sig":{signal}}}"#);
        send_ctl_op(&dir, &op);
        return Ok(());
    }

    // No live ctl endpoint but pid recorded — best-effort direct signal.
    if let Some(pid) = read_pid_file(&dir) {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, signal as libc::c_int);
        }
        #[cfg(windows)]
        {
            // WIN-PATH: Windows has no signal model. The supervisor's ctl path is
            // the supported transport; a direct numeric signal is not meaningful
            // off the ctl.sock. Validatable only on a real Windows box.
            let _ = pid;
        }
        return Ok(());
    }

    Err(LightrError::InvalidRef(format!(
        "run {id}: no control endpoint or pid to signal"
    )))
}

/// Block until the run exits, returning its exit code. Polls the status file the
/// supervisor writes on child exit (`exited <code>`) — the same source `ps` and
/// `stop` read. If the run is ALREADY exited, returns immediately. A run whose
/// supervisor vanished without writing a final status (no live endpoint, no
/// `exited` line) is detected and surfaced as an honest error rather than
/// blocking forever.
pub fn wait_run(home: &Path, id: &str) -> Result<i32> {
    use std::time::Duration;

    let dir = run_dir(home, id);
    if !dir.exists() {
        return Err(LightrError::RefNotFound(id.to_string()));
    }

    loop {
        // Terminal status wins immediately (covers the already-exited case).
        if let Some(code) = read_status_file(&dir)
            .as_deref()
            .and_then(parse_exit_code_from_status)
        {
            return Ok(code);
        }

        if !is_running(&dir) {
            // Not running and no parseable exit code: re-check the status once
            // (a race between the supervisor's final write and the endpoint
            // teardown), then fail closed rather than spin forever.
            if let Some(code) = read_status_file(&dir)
                .as_deref()
                .and_then(parse_exit_code_from_status)
            {
                return Ok(code);
            }
            return Err(LightrError::InvalidRef(format!(
                "run {id} is not running and has no recorded exit code"
            )));
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

/// WP-D (container prune): list every STOPPED run id under `<home>/run` — the
/// candidates `docker container prune` removes. A run is "stopped" when its
/// `status` file starts with `exited` (the supervisor's terminal write). The
/// `names` registry sub-dir is skipped (it is not a run). A run that is still
/// RUNNING, or one with no/partial status (a freshly-created or vanished
/// supervisor), is NOT listed — fail-closed: prune touches only proven-exited
/// runs, never a live one or an indeterminate one.
///
/// `home` is INJECTED (like the other primitives) so the unit tests pass a
/// private tempdir. Mirrors the `<home>/run` walk in `registry`/`ps` (no public
/// helper exists to reuse).
pub fn list_stopped_runs(home: &Path) -> Result<Vec<String>> {
    let root = home.join("run");
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = match path.file_name().map(|s| s.to_string_lossy().into_owned()) {
            Some(n) => n,
            None => continue,
        };
        // The name→id registry sub-dir is not a run.
        if id == "names" {
            continue;
        }
        // Fail-closed: only a run whose status file proves it `exited` is a prune
        // candidate. A running run still has a live endpoint, so even if a stale
        // status lingered, re-check liveness and exclude it.
        let exited = read_status_file(&path)
            .map(|s| s.starts_with("exited"))
            .unwrap_or(false);
        if exited && !is_running(&path) {
            ids.push(id);
        }
    }
    Ok(ids)
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
