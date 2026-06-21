//! `lightr pause` handler — suspend one or more running containers (docker pause).
//!
//! Faithful to `docker pause`: each target is SIGSTOP'd via the run's control
//! endpoint (the same `signal_run` primitive `kill` uses). SIGSTOP is the
//! POSIX job-control "stop" signal — its NUMBER differs across platforms
//! (Linux 19, macOS 17), so we source it from `libc::SIGSTOP` under
//! `#[cfg(unix)]` rather than hardcoding a number. Every target is processed
//! (continue-on-error); the exit code summarises the batch.

#[cfg(unix)]
use crate::lightr_home;

/// The raw SIGSTOP signal number for this platform. `libc::SIGSTOP` is the
/// honest, platform-correct constant (Linux 19, macOS 17) — never a hardcoded
/// number that would be wrong on the other unix.
#[cfg(unix)]
const SIGSTOP: i32 = libc::SIGSTOP;

pub fn run(targets: &[String]) -> i32 {
    // Empty target list is a usage error (docker pause requires ≥1 container).
    if targets.is_empty() {
        eprintln!("Error: \"pause\" requires at least 1 argument");
        return 2;
    }

    // WIN-PATH: Windows has no POSIX job-control signal model. `signal_run`'s
    // ctl path is the supported transport but SIGSTOP has no honest Windows
    // number — fail closed rather than send a meaningless signal.
    #[cfg(not(unix))]
    {
        let _ = targets;
        eprintln!("Error: pause is not supported on this host (no POSIX signal model)");
        1
    }

    #[cfg(unix)]
    {
        let home = lightr_home();
        let mut any_failure = false;

        for target in targets {
            let id = match lightr_run::resolve(&home, target) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    any_failure = true;
                    continue;
                }
            };

            match lightr_run::signal_run(&home, &id, SIGSTOP) {
                Ok(()) => println!("{target}"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    any_failure = true;
                }
            }
        }

        if any_failure {
            1
        } else {
            0
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::SIGSTOP;

    #[test]
    fn sigstop_is_the_libc_platform_constant() {
        // SIGSTOP differs by platform (Linux 19, macOS 17). We assert it equals
        // libc's constant — the contract is "source from libc", not a number.
        assert_eq!(SIGSTOP, libc::SIGSTOP);
        // Guard the documented per-platform values so a wrong import is caught
        // (these also prove it is a real, positive signal number).
        #[cfg(target_os = "linux")]
        assert_eq!(SIGSTOP, 19);
        #[cfg(target_os = "macos")]
        assert_eq!(SIGSTOP, 17);
    }

    #[test]
    fn empty_targets_is_usage_error() {
        assert_eq!(super::run(&[]), 2);
    }
}
