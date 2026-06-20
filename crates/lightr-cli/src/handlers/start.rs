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

use crate::{exit::die_lightr, lightr_home};

pub fn run(targets: &[String]) -> i32 {
    if targets.is_empty() {
        eprintln!("lightr: \"start\" requires at least 1 argument");
        return 2;
    }

    let home = lightr_home();
    let mut any_failed = false;

    for token in targets {
        // ref -> id
        let id = match resolve(&home, token) {
            Ok(id) => id,
            Err(e) => {
                // die_lightr prints `lightr: <msg>` and returns its mapped code;
                // we discard the code (continue-on-error) and only flag failure.
                let _ = die_lightr(&e);
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
}
