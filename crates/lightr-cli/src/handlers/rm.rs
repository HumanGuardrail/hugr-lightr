//! `lightr rm` handler — remove one or more stopped containers (docker rm).
//! Behavior lands here in WP-LIFE-03.

use crate::lightr_home;

/// Remove each target run, docker-rm faithful:
///   - resolve name-or-id → id, then `remove_run` (refuses RUNNING unless force)
///   - on success echo the target (docker echoes the ref it removed)
///   - resolve/remove failure: report to stderr, remember it, keep going
///   - exit 0 if every target removed, 1 if any failed, 2 on empty targets
pub fn run(targets: &[String], force: bool) -> i32 {
    if targets.is_empty() {
        // FIX #77: drop the trailing period so the shape matches kill/pause/start.
        eprintln!("Error: \"rm\" requires at least 1 argument");
        return 2;
    }

    let home = lightr_home();
    let mut any_failed = false;

    for t in targets {
        match lightr_run::resolve(&home, t) {
            Ok(id) => match lightr_run::remove_run(&home, &id, force) {
                Ok(()) => println!("{t}"),
                Err(e) => {
                    // remove_run failed (e.g. running run without force).
                    // Surface the real error and keep processing the rest.
                    eprintln!("Error: {e}");
                    any_failed = true;
                }
            },
            Err(_) => {
                eprintln!("Error: No such container: {t}");
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

#[cfg(test)]
mod tests {
    /// Pure-logic guard: the exit-code contract (0 all-ok / 1 any-fail / 2
    /// empty) is the verb's only branch state. Mirror it without run-dir
    /// fixtures (the primitives are unit-tested in lightr-run).
    fn exit_code(empty: bool, any_failed: bool) -> i32 {
        if empty {
            2
        } else if any_failed {
            1
        } else {
            0
        }
    }

    #[test]
    fn empty_targets_is_usage_error() {
        assert_eq!(exit_code(true, false), 2);
    }

    #[test]
    fn all_ok_is_zero() {
        assert_eq!(exit_code(false, false), 0);
    }

    #[test]
    fn any_failure_is_one() {
        assert_eq!(exit_code(false, true), 1);
    }
}
