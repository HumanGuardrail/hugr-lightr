//! `lightr start` handler — start one or more stopped containers (docker start).
//!
//! Docker-faithful semantics: for each target, resolve the ref to a run id,
//! check liveness, and re-spawn it in place from its on-disk spec. A target
//! that is already `Running` is a no-op success (docker echoes the name and
//! moves on). Targets are processed independently — a failure on one never
//! halts the rest (continue-on-error). The name is echoed to stdout only on
//! success, matching `docker start`. Exit 0 if every target succeeded, 1 if
//! any failed, 2 if no targets were given.

use lightr_run::{resolve, respawn_run, run_status, RunStatus};

use crate::{
    exit::{die_lightr, die_resolve},
    lightr_home,
};

pub fn run(targets: &[String]) -> i32 {
    if targets.is_empty() {
        // FIX #77: standardize the prefix to `Error:` (was `lightr:`).
        eprintln!("Error: \"start\" requires at least 1 argument");
        return 2;
    }

    let home = lightr_home();
    let mut any_failed = false;

    for token in targets {
        // ref -> id
        let id = match resolve(&home, token) {
            Ok(id) => id,
            Err(e) => {
                // No-such-container resolution path → Docker parity (exit 1),
                // honest "No such container" message (WP-EXIT-CODE). The code is
                // discarded (continue-on-error); the batch flags failure → 1.
                let _ = die_resolve(&e, token);
                any_failed = true;
                continue;
            }
        };

        // Already running -> docker start is a no-op success.
        match run_status(&home, &id) {
            Ok(RunStatus::Running) => {
                println!("{token}");
                continue;
            }
            Ok(_) => { /* Exited/Unknown -> proceed to re-spawn */ }
            Err(e) => {
                let _ = die_lightr(&e);
                any_failed = true;
                continue;
            }
        }

        // Re-spawn the stopped run in its same dir from SpecOnDisk.
        match respawn_run(&home, &id) {
            Ok(()) => println!("{token}"),
            Err(e) => {
                let _ = die_lightr(&e);
                any_failed = true;
            }
        }
    }

    i32::from(any_failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_targets_is_usage_error() {
        assert_eq!(run(&[]), 2);
    }

    /// Docker parity (WP-EXIT-CODE): `start <missing>` → exit 1, not 2.
    /// Arg-error path (empty targets, above) stays 2.
    #[test]
    fn missing_container_exits_1() {
        let _g = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        std::env::set_var("LIGHTR_HOME", tmp.path());
        let code = run(&["does-not-exist".to_string()]);
        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 1, "start on a missing container must exit 1");
    }
}
