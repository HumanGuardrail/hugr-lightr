//! `lightr kill` handler — send a signal to a running container (docker kill).
//!
//! Faithful to `docker kill`: default signal is SIGKILL; `--signal` accepts a
//! numeric value or a portable signal name. Every target is processed
//! (continue-on-error); the exit code summarises the batch.

use crate::lightr_home;

/// Resolve a user-supplied signal spec to its raw signal number.
///
/// Accepts a bare decimal number (`"9"`, `"15"`) OR one of the five
/// POSIX-portable signal names whose numbers are stable across macOS and
/// Linux, case-insensitive and with an optional `SIG` prefix:
/// HUP=1, INT=2, QUIT=3, KILL=9, TERM=15.
///
/// Platform-specific signals (USR1, STOP, …) are deliberately NOT mapped by
/// name — their numbers differ across platforms — but remain reachable as an
/// explicit numeric. An unrecognised spec yields `None`.
fn parse_signal(spec: &str) -> Option<i32> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    // Numeric form: any non-negative decimal signal number.
    if let Ok(n) = spec.parse::<i32>() {
        if n >= 0 {
            return Some(n);
        }
        return None;
    }

    // Portable name form: case-insensitive, optional SIG prefix.
    let upper = spec.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    match name {
        "HUP" => Some(1),
        "INT" => Some(2),
        "QUIT" => Some(3),
        "KILL" => Some(9),
        "TERM" => Some(15),
        _ => None,
    }
}

pub fn run(targets: &[String], signal: Option<&str>) -> i32 {
    // Empty target list is a usage error (docker kill requires ≥1 container).
    if targets.is_empty() {
        eprintln!("Error: \"kill\" requires at least 1 argument");
        return 2;
    }

    // Default signal is SIGKILL (9), matching `docker kill`.
    let sig = match signal {
        None => 9,
        Some(s) => match parse_signal(s) {
            Some(n) => n,
            None => {
                eprintln!("Error: invalid signal: {s}");
                return 2;
            }
        },
    };

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

        match lightr_run::signal_run(&home, &id, sig) {
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

#[cfg(test)]
mod tests {
    use super::parse_signal;

    #[test]
    fn numeric_signals_pass_through() {
        assert_eq!(parse_signal("9"), Some(9));
        assert_eq!(parse_signal("15"), Some(15));
        assert_eq!(parse_signal("0"), Some(0));
        // Platform-specific numbers are accepted as explicit numerics.
        assert_eq!(parse_signal("30"), Some(30));
    }

    #[test]
    fn portable_names_map_to_stable_numbers() {
        assert_eq!(parse_signal("HUP"), Some(1));
        assert_eq!(parse_signal("INT"), Some(2));
        assert_eq!(parse_signal("QUIT"), Some(3));
        assert_eq!(parse_signal("KILL"), Some(9));
        assert_eq!(parse_signal("TERM"), Some(15));
    }

    #[test]
    fn names_are_case_insensitive_and_sig_prefix_optional() {
        assert_eq!(parse_signal("kill"), Some(9));
        assert_eq!(parse_signal("Term"), Some(15));
        assert_eq!(parse_signal("SIGKILL"), Some(9));
        assert_eq!(parse_signal("sigterm"), Some(15));
        assert_eq!(parse_signal("SigHup"), Some(1));
        assert_eq!(parse_signal("  TERM  "), Some(15));
    }

    #[test]
    fn unknown_or_invalid_specs_reject() {
        // Platform-specific names are NOT mapped (avoid guessing numbers).
        assert_eq!(parse_signal("USR1"), None);
        assert_eq!(parse_signal("STOP"), None);
        assert_eq!(parse_signal("SIGUSR1"), None);
        // Garbage and negatives reject.
        assert_eq!(parse_signal("nope"), None);
        assert_eq!(parse_signal(""), None);
        assert_eq!(parse_signal("-1"), None);
        assert_eq!(parse_signal("9x"), None);
    }
}
