//! `lightr unpause` handler — resume one or more paused containers (docker unpause).
//!
//! The mirror of `pause`: each target is SIGCONT'd via the run's control
//! endpoint (the same `signal_run` primitive `kill`/`pause` use). SIGCONT is the
//! POSIX job-control "continue" signal — its NUMBER differs across platforms
//! (Linux 18, macOS 19), so we source it from `libc::SIGCONT` under
//! `#[cfg(unix)]` rather than hardcoding a number. Every target is processed
//! (continue-on-error); the exit code summarises the batch.

#[cfg(unix)]
use crate::lightr_home;

/// The raw SIGCONT signal number for this platform. `libc::SIGCONT` is the
/// honest, platform-correct constant (Linux 18, macOS 19) — never a hardcoded
/// number that would be wrong on the other unix.
#[cfg(unix)]
const SIGCONT: i32 = libc::SIGCONT;

pub fn run(targets: &[String]) -> i32 {
    // Empty target list is a usage error (docker unpause requires ≥1 container).
    if targets.is_empty() {
        eprintln!("Error: \"unpause\" requires at least 1 argument");
        return 2;
    }

    // WIN-PATH: Windows has no POSIX job-control signal model. SIGCONT has no
    // honest Windows number — fail closed rather than send a meaningless signal.
    #[cfg(not(unix))]
    {
        let _ = targets;
        eprintln!("Error: unpause is not supported on this host (no POSIX signal model)");
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

            match lightr_run::signal_run(&home, &id, SIGCONT) {
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
    use super::SIGCONT;

    #[test]
    fn sigcont_is_the_libc_platform_constant() {
        // SIGCONT differs by platform (Linux 18, macOS 19). We assert it equals
        // libc's constant — the contract is "source from libc", not a number.
        assert_eq!(SIGCONT, libc::SIGCONT);
        // Guard the documented per-platform values so a wrong import is caught
        // (these also prove it is a real, positive signal number).
        #[cfg(target_os = "linux")]
        assert_eq!(SIGCONT, 18);
        #[cfg(target_os = "macos")]
        assert_eq!(SIGCONT, 19);
    }

    #[test]
    fn empty_targets_is_usage_error() {
        assert_eq!(super::run(&[]), 2);
    }
}
