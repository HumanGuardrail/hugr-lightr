//! Healthcheck probe for a run (F-309 + WP-RC-4).
//!
//! build-spec-parity.md §0 (memo-key law) + §4.2 (WP-A3) + WP-RC-4 (wire the
//! Docker `--health-*` flags that were parsed-and-discarded).
//!
//! A healthcheck is a **post-result probe**: it observes a running process,
//! it is never part of the command's deterministic output, so it is
//! deliberately **NOT** in the memo key (§0). It exists to surface liveness via
//! `ps`, not to gate caching.
//!
//! Two shapes:
//!  * **Detached** runs (`spawn_detached_with_health` → supervisor): after the
//!    child is spawned, the supervisor waits out `start_period_s`, then probes
//!    on `interval_s`, each probe capped at `timeout_s`, and drives the
//!    [`HealthState`] machine (starting → healthy/unhealthy, with a failing
//!    streak), writing the verdict to `<run_dir>/health` so `ps` can surface it.
//!  * **Foreground** runs (`--health-cmd` without `-d`): the healthcheck is a
//!    supervisor-only feature — the CLI accepts the flags but emits an honest
//!    note that the probe runs only for `-d` runs (no loop, no silent no-op).

use lightr_core::Result;
use std::path::Path;

/// A run's healthcheck configuration (mirrors Docker `HEALTHCHECK`).
///
/// `cmd` is run via the system shell in the run's cwd; a zero exit ⇒ a passing
/// probe. `interval_s` is the supervisor's wait between probe rounds;
/// `timeout_s` caps a single probe (a probe that outlives it is a failure, like
/// Docker `--health-timeout`); `start_period_s` is the grace window after start
/// during which a failing probe does NOT count against the retry budget (Docker
/// `--health-start-period`); `retries` is the number of CONSECUTIVE failing
/// probes (after the start period) tolerated before the state flips to
/// `Unhealthy` (Docker semantics: a transient blip does not immediately mark the
/// container unhealthy).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Healthcheck {
    pub cmd: String,
    pub interval_s: u64,
    pub timeout_s: u64,
    pub start_period_s: u64,
    pub retries: u32,
}

impl Healthcheck {
    /// Construct a [`Healthcheck`] with Docker's default timings for any field a
    /// caller does not override: interval 30s, timeout 30s, start-period 0s,
    /// retries 3. Keeps the CLI/compose call-sites terse.
    pub fn new(cmd: String) -> Self {
        Healthcheck {
            cmd,
            interval_s: 30,
            timeout_s: 30,
            start_period_s: 0,
            retries: 3,
        }
    }
}

/// The health state surfaced to `ps` / the CLI.
///
/// `Starting` is the Docker "health: starting" state a container reports during
/// its start period before any verdict has been reached. It is written to the
/// health file so `ps` can distinguish "not yet probed" from "probed healthy".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Starting,
    Healthy,
    Unhealthy,
}

impl Health {
    /// The on-disk string written to `<run_dir>/health` and read by `ps`.
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Starting => "starting",
            Health::Healthy => "healthy",
            Health::Unhealthy => "unhealthy",
        }
    }
}

/// The supervisor's health state machine (Docker parity).
///
/// Each interval the supervisor calls [`HealthState::record`] with the verdict
/// of one probe round (one [`probe_once`] call). The machine starts in
/// `Starting`, flips to `Healthy` on the first success, and only flips to
/// `Unhealthy` after `retries + 1` CONSECUTIVE failures — but a failure inside
/// the start period (the first `start_period_s` after launch) never counts
/// against the budget, matching Docker's `--health-start-period`.
/// `failing_streak` is the count of consecutive failures, reset to 0 on any
/// success (the Docker `FailingStreak`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthState {
    pub status: Health,
    pub failing_streak: u32,
}

impl Default for HealthState {
    fn default() -> Self {
        HealthState {
            status: Health::Starting,
            failing_streak: 0,
        }
    }
}

impl HealthState {
    /// Fold one probe round into the state.
    ///
    /// * `passed` — did this round succeed (a single [`probe_once`] verdict).
    /// * `in_start_period` — is the probe still inside the grace window. A
    ///   failure here is recorded in `failing_streak` (Docker surfaces it) but
    ///   never flips the status to `Unhealthy`: the container stays `Starting`
    ///   until the first success or the first post-grace failure breaches the
    ///   budget.
    /// * `retries` — the configured tolerated consecutive failures.
    pub fn record(&mut self, passed: bool, in_start_period: bool, retries: u32) {
        if passed {
            self.failing_streak = 0;
            self.status = Health::Healthy;
            return;
        }
        // A failing probe.
        self.failing_streak = self.failing_streak.saturating_add(1);
        if in_start_period {
            // Still in the grace window: never go Unhealthy yet. Keep the
            // current status (Starting on a cold container; a prior Healthy is
            // kept — Docker does not demote during the start period either, the
            // streak just accrues).
            return;
        }
        // Past the grace window: Unhealthy once the streak breaches retries + 1
        // consecutive failures.
        if self.failing_streak >= retries.saturating_add(1) {
            self.status = Health::Unhealthy;
        }
    }
}

/// Run the healthcheck command once in `cwd`, capped at `timeout_s` seconds.
/// Returns `true` iff it exited 0 within the timeout.
///
/// The command is executed through the platform shell (`/bin/sh -c` on unix,
/// `cmd /C` on Windows) so a user can write a real shell snippet (the Docker
/// `HEALTHCHECK CMD` model). stdin/stdout/stderr are discarded — only the exit
/// status matters. A probe that outlives `timeout_s` is killed and counts as a
/// failure (Docker `--health-timeout`); `timeout_s == 0` means no timeout.
fn run_once(hc: &Healthcheck, cwd: &Path) -> bool {
    #[cfg(unix)]
    let mut command = {
        let mut c = std::process::Command::new("/bin/sh");
        c.arg("-c").arg(&hc.cmd);
        c
    };
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(&hc.cmd);
        c
    };
    #[cfg(not(any(unix, windows)))]
    let mut command = std::process::Command::new(&hc.cmd);

    command
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // No timeout configured ⇒ the original blocking behaviour (status()).
    if hc.timeout_s == 0 {
        return match command.status() {
            Ok(status) => status.success(),
            // A probe we cannot even spawn (bad shell, denied) is a failing
            // probe, never a panic: liveness is best-effort and never crashes.
            Err(_) => false,
        };
    }

    // Timeout configured: spawn, poll for completion, kill on overrun. A probe
    // we cannot spawn is a failing probe (not a panic).
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(hc.timeout_s);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // Overran the timeout: kill it and report failure. The kill
                    // is best-effort; the verdict is a failed probe regardless.
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            // try_wait error: treat as a failed, finished probe.
            Err(_) => return false,
        }
    }
}

/// Probe the healthcheck ONCE (a single round), capped at `timeout_s`.
///
/// This is the per-interval verdict the supervisor folds into the
/// [`HealthState`] machine — exactly one shell invocation. The retry budget is
/// the machine's job (consecutive rounds), NOT this function's: a single round
/// is healthy iff the one command succeeds within the timeout. Returns `true`
/// for a passing round.
pub fn probe_once(hc: &Healthcheck, cwd: &Path) -> bool {
    run_once(hc, cwd)
}

/// Probe the healthcheck, honoring `retries` IN A SINGLE CALL: run the command
/// up to `retries + 1` times, returning [`Health::Healthy`] on the FIRST
/// success and [`Health::Unhealthy`] only if every attempt fails.
///
/// Retained for the foreground one-shot verdict (no state machine, no start
/// period). The detached supervisor uses [`probe_once`] + [`HealthState`]
/// instead, so a transient blip across SEPARATE intervals is tolerated the
/// Docker way. It does not sleep between attempts.
pub fn probe(hc: &Healthcheck, cwd: &Path) -> Health {
    let attempts = hc.retries.saturating_add(1);
    for _ in 0..attempts {
        if run_once(hc, cwd) {
            return Health::Healthy;
        }
    }
    Health::Unhealthy
}

/// Write the current health state to `<run_dir>/health` (best-effort).
///
/// `ps` reads this file to surface liveness. A write failure is swallowed: the
/// health file is a status hint, never a correctness gate, and must not abort
/// the supervisor loop.
pub fn write_state(run_dir: &Path, health: Health) {
    let _ = std::fs::write(run_dir.join("health"), health.as_str());
}

/// Read the health state from `<run_dir>/health`, if present and valid.
pub fn read_state(run_dir: &Path) -> Option<Health> {
    let raw = std::fs::read_to_string(run_dir.join("health")).ok()?;
    match raw.trim() {
        "starting" => Some(Health::Starting),
        "healthy" => Some(Health::Healthy),
        "unhealthy" => Some(Health::Unhealthy),
        _ => None,
    }
}

/// Read a [`Healthcheck`] persisted alongside the spec, if any.
///
/// Returns `Ok(None)` when no healthcheck file exists (the common case);
/// `Ok(Some(_))` when one is present and parses. The on-disk form is the JSON
/// written by [`save_for`] from `spawn_detached_with_health` before it forks the
/// supervisor. `timeout_s`/`start_period_s` are serde-defaulted, so a
/// `healthcheck.json` written before WP-RC-4 (only cmd/interval/retries) still
/// loads — the missing fields take Docker's defaults (timeout 30s, start 0s).
pub fn load_for(run_dir: &Path) -> Result<Option<Healthcheck>> {
    let path = run_dir.join("healthcheck.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(lightr_core::LightrError::Io(e)),
    };
    let parsed: HealthcheckOnDisk = serde_json::from_slice(&bytes).map_err(|e| {
        lightr_core::LightrError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    })?;
    Ok(Some(Healthcheck {
        cmd: parsed.cmd,
        interval_s: parsed.interval_s,
        timeout_s: parsed.timeout_s,
        start_period_s: parsed.start_period_s,
        retries: parsed.retries,
    }))
}

/// Persist a [`Healthcheck`] for the supervisor to pick up.
pub fn save_for(run_dir: &Path, hc: &Healthcheck) -> Result<()> {
    let on_disk = HealthcheckOnDisk {
        cmd: hc.cmd.clone(),
        interval_s: hc.interval_s,
        timeout_s: hc.timeout_s,
        start_period_s: hc.start_period_s,
        retries: hc.retries,
    };
    let bytes = serde_json::to_vec(&on_disk)
        .map_err(|e| lightr_core::LightrError::Io(std::io::Error::other(e)))?;
    std::fs::write(run_dir.join("healthcheck.json"), &bytes).map_err(lightr_core::LightrError::Io)
}

/// Serde default for [`HealthcheckOnDisk::timeout_s`] — Docker's 30s probe cap,
/// applied when reading a `healthcheck.json` written before the field existed.
fn default_timeout_s() -> u64 {
    30
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HealthcheckOnDisk {
    cmd: String,
    interval_s: u64,
    // serde-defaulted for back-compat with pre-WP-RC-4 healthcheck.json that
    // only had cmd/interval/retries.
    #[serde(default = "default_timeout_s")]
    timeout_s: u64,
    #[serde(default)]
    start_period_s: u64,
    retries: u32,
}

#[cfg(test)]
#[path = "healthcheck_tests.rs"]
mod tests;
