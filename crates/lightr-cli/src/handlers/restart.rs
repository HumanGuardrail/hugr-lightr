//! `lightr restart` handler — restart one or more containers (docker restart).
//! Faithful to `docker restart`: graceful SIGTERM, wait up to `grace` seconds
//! for the run to stop, escalate to SIGKILL if it won't, then respawn it.
//! Already-stopped runs are simply respawned. Continue-on-error across all
//! targets; the loop is bounded so it can never hang.

use std::thread::sleep;
use std::time::Duration;

use lightr_run::{resolve, respawn_run, run_status, signal_run, RunStatus};

use crate::{
    exit::{die_lightr, die_resolve},
    lightr_home,
};

/// POSIX signal numbers (portable subset we issue).
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

/// Poll cadence while waiting for a run to leave the Running state.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Polls per second at the cadence above (1000ms / 100ms).
const POLLS_PER_SEC: u64 = 10;
/// Hard cap on SIGKILL-confirmation polls (~1s) so escalation can't hang.
const KILL_POLLS: u64 = 10;

pub fn run(targets: &[String], grace: u64) -> i32 {
    if targets.is_empty() {
        eprintln!("lightr: restart requires at least one container");
        return 2;
    }

    let home = lightr_home();
    let mut any_failed = false;

    for token in targets {
        match restart_one(&home, token, grace) {
            Ok(name) => println!("{name}"),
            Err(code) => {
                // restart_one already printed the diagnostic; remember the
                // failure but keep processing the remaining targets.
                debug_assert_ne!(code, 0);
                any_failed = true;
            }
        }
    }

    if any_failed {
        1
    } else {
        0
    }
}

/// Restart a single target. On success returns the resolved id (printed by the
/// caller). On failure the diagnostic is already emitted and a non-zero code is
/// returned so the caller can remember the failure.
fn restart_one(home: &std::path::Path, token: &str, grace: u64) -> Result<String, i32> {
    // No-such-container resolution path → Docker parity (exit 1, "No such
    // container"); InvalidRef stays usage-class 2 (WP-EXIT-CODE).
    let id = resolve(home, token).map_err(|e| die_resolve(&e, token))?;

    let status = run_status(home, &id).map_err(|e| die_lightr(&e))?;

    if matches!(status, RunStatus::Running) {
        graceful_stop(home, &id, grace).map_err(|e| die_lightr(&e))?;
    }
    // Exited/Unknown: nothing alive to stop — fall through to respawn.

    respawn_run(home, &id).map_err(|e| die_lightr(&e))?;
    Ok(id)
}

/// SIGTERM → poll up to `grace` seconds → SIGKILL → brief confirm poll.
/// Bounded: total polls are capped so this can never spin forever.
fn graceful_stop(
    home: &std::path::Path,
    id: &str,
    grace: u64,
) -> Result<(), lightr_core::LightrError> {
    signal_run(home, id, SIGTERM)?;

    // grace seconds worth of polls; saturating so a huge grace can't overflow.
    let grace_polls = grace.saturating_mul(POLLS_PER_SEC);
    if wait_until_stopped(home, id, grace_polls)? {
        return Ok(());
    }

    // Still running after the grace window — escalate.
    signal_run(home, id, SIGKILL)?;
    let _ = wait_until_stopped(home, id, KILL_POLLS)?;
    // Whether or not the brief confirm poll saw it leave Running, we've issued
    // SIGKILL and respawn follows; respawn is the authority on a clean slot.
    Ok(())
}

/// Poll `run_status` every `POLL_INTERVAL` up to `max_polls` times. Returns
/// `Ok(true)` as soon as the run is no longer Running, `Ok(false)` if it is
/// still Running after the cap. Bounded by construction.
fn wait_until_stopped(
    home: &std::path::Path,
    id: &str,
    max_polls: u64,
) -> Result<bool, lightr_core::LightrError> {
    for _ in 0..max_polls {
        if !matches!(run_status(home, id)?, RunStatus::Running) {
            return Ok(true);
        }
        sleep(POLL_INTERVAL);
    }
    // Final check after the last sleep so a max_polls of 0 still answers.
    Ok(!matches!(run_status(home, id)?, RunStatus::Running))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_targets_is_exit_2() {
        assert_eq!(run(&[], 10), 2);
    }

    /// Docker parity (WP-EXIT-CODE): `restart <missing>` → exit 1, not 2.
    /// Arg-error path (empty targets, above) stays 2.
    #[test]
    fn missing_container_exits_1() {
        let _g = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        std::env::set_var("LIGHTR_HOME", tmp.path());
        let code = run(&["does-not-exist".to_string()], 10);
        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 1, "restart on a missing container must exit 1");
    }
}
