//! Healthcheck probe for a run (F-309).
//!
//! build-spec-parity.md §0 (memo-key law) + §4.2 (WP-A3).
//!
//! A healthcheck is a **post-result probe**: it observes a running process,
//! it is never part of the command's deterministic output, so it is
//! deliberately **NOT** in the memo key (§0). It exists to surface liveness via
//! `ps`, not to gate caching.
//!
//! Two shapes:
//!  * **Detached** runs (`spawn_detached` → supervisor): after the child is
//!    spawned, the supervisor probes on an interval and writes the current
//!    state to `<run_dir>/health` so `ps` can surface `Healthy`/`Unhealthy`.
//!  * **Foreground** runs (`--health-cmd` without detach): one probe is run
//!    post-exit and reported by the CLI; no loop (handled by the CLI layer).

use lightr_core::Result;
use std::path::Path;

/// A run's healthcheck configuration.
///
/// `cmd` is run via the system shell in the run's cwd; a zero exit ⇒ a passing
/// probe. `interval_s` is the supervisor's wait between probe rounds;
/// `retries` is the number of CONSECUTIVE failing probes tolerated before the
/// state flips to `Unhealthy` (Docker semantics: a transient blip does not
/// immediately mark the container unhealthy).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Healthcheck {
    pub cmd: String,
    pub interval_s: u64,
    pub retries: u32,
}

/// The health state surfaced to `ps` / the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Unhealthy,
}

impl Health {
    /// The on-disk string written to `<run_dir>/health` and read by `ps`.
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Healthy => "healthy",
            Health::Unhealthy => "unhealthy",
        }
    }
}

/// Run the healthcheck command once in `cwd`. Returns `true` iff it exited 0.
///
/// The command is executed through the platform shell (`/bin/sh -c` on unix,
/// `cmd /C` on Windows) so a user can write a real shell snippet (the Docker
/// `HEALTHCHECK CMD` model). stdin/stdout/stderr are discarded — only the exit
/// status matters.
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

    match command.status() {
        Ok(status) => status.success(),
        // A probe we cannot even spawn (bad shell, denied) is a failing probe,
        // never a panic: liveness is best-effort and must not crash the loop.
        Err(_) => false,
    }
}

/// Probe the healthcheck, honoring `retries`: run the command up to
/// `retries + 1` times, returning [`Health::Healthy`] on the FIRST success and
/// [`Health::Unhealthy`] only if every attempt fails.
///
/// This is the single-round verdict the supervisor records each interval, and
/// the one-shot verdict a foreground `--health-cmd` reports post-exit. It does
/// not sleep between attempts (the supervisor owns the inter-round interval).
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
        "healthy" => Some(Health::Healthy),
        "unhealthy" => Some(Health::Unhealthy),
        _ => None,
    }
}

/// Read a [`Healthcheck`] persisted alongside the spec, if any.
///
/// Returns `Ok(None)` when no healthcheck file exists (the common case);
/// `Ok(Some(_))` when one is present and parses. The on-disk form is the JSON
/// `{ "cmd": ..., "interval_s": ..., "retries": ... }` written by [`save_for`]
/// from `spawn_detached_with_health` before it forks the supervisor.
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
        retries: parsed.retries,
    }))
}

/// Persist a [`Healthcheck`] for the supervisor to pick up.
pub fn save_for(run_dir: &Path, hc: &Healthcheck) -> Result<()> {
    let on_disk = HealthcheckOnDisk {
        cmd: hc.cmd.clone(),
        interval_s: hc.interval_s,
        retries: hc.retries,
    };
    let bytes = serde_json::to_vec(&on_disk)
        .map_err(|e| lightr_core::LightrError::Io(std::io::Error::other(e)))?;
    std::fs::write(run_dir.join("healthcheck.json"), &bytes).map_err(lightr_core::LightrError::Io)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HealthcheckOnDisk {
    cmd: String,
    interval_s: u64,
    retries: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    // probe returns Healthy for a command that exits 0.
    #[test]
    fn probe_healthy_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let hc = Healthcheck {
            cmd: "exit 0".to_string(),
            interval_s: 1,
            retries: 0,
        };
        assert_eq!(probe(&hc, tmp.path()), Health::Healthy);
    }

    // probe flips Healthy → Unhealthy on a failing command (all retries fail).
    #[test]
    fn probe_unhealthy_on_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let healthy = Healthcheck {
            cmd: "true".to_string(),
            interval_s: 1,
            retries: 2,
        };
        assert_eq!(
            probe(&healthy, tmp.path()),
            Health::Healthy,
            "a passing cmd must report Healthy"
        );

        let failing = Healthcheck {
            cmd: "exit 1".to_string(),
            interval_s: 1,
            retries: 2,
        };
        assert_eq!(
            probe(&failing, tmp.path()),
            Health::Unhealthy,
            "a cmd that always fails must report Unhealthy after retries"
        );
    }

    // write_state / read_state round-trip via the run dir's `health` file.
    #[test]
    fn health_state_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_state(tmp.path()), None, "no file ⇒ None");

        write_state(tmp.path(), Health::Healthy);
        assert_eq!(read_state(tmp.path()), Some(Health::Healthy));

        write_state(tmp.path(), Health::Unhealthy);
        assert_eq!(read_state(tmp.path()), Some(Health::Unhealthy));
    }

    // save_for / load_for round-trip the Healthcheck config.
    #[test]
    fn healthcheck_persist_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load_for(tmp.path()).unwrap(), None, "no file ⇒ Ok(None)");

        let hc = Healthcheck {
            cmd: "curl -fsS localhost:8080/health".to_string(),
            interval_s: 15,
            retries: 5,
        };
        save_for(tmp.path(), &hc).unwrap();
        assert_eq!(load_for(tmp.path()).unwrap(), Some(hc));
    }
}
