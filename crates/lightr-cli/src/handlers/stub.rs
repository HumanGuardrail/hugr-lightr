//! Honest fail-closed helper for CLI-surface-freeze stubs.
//!
//! The Docker-parity campaign froze the CLI surface (verbs, subcommands, flags)
//! before the feature WPs land. Every frozen-but-unimplemented path routes here:
//! it returns a `LightrError::Io(Unsupported)` so the exit code is 1 (runtime
//! error, NOT silent success, NOT a usage error) and prints a one-line message
//! naming the owning WP. NEVER let a frozen verb/flag no-op silently.

use lightr_core::LightrError;

use crate::exit::die_lightr;

/// Build the honest "not yet implemented" error for `what`, naming the WP.
pub fn not_yet(what: &str, wp: &str) -> LightrError {
    LightrError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{what}: not yet implemented ({wp})"),
    ))
}

/// Print the honest stub error to stderr and return its mapped exit code (1).
pub fn stub(what: &str, wp: &str) -> i32 {
    die_lightr(&not_yet(what, wp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_yet_names_the_wp_and_verb() {
        let e = not_yet("rm", "WP-LIFE-03");
        let msg = e.to_string();
        assert!(msg.contains("rm"), "message must name the verb");
        assert!(msg.contains("WP-LIFE-03"), "message must name the WP");
        assert!(
            msg.contains("not yet implemented"),
            "message must be honest about being unimplemented"
        );
    }

    #[test]
    fn stub_exits_1_not_0_and_not_2() {
        // Fail-closed: runtime error (1), never silent success (0) or usage (2).
        let code = stub("kill", "WP-LIFE-03");
        assert_eq!(code, 1, "stub must exit 1 (honest runtime error)");
    }
}
