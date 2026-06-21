//! Exit-code mapping per build-spec v2 §7.
//!
//! Law:
//! - Ok paths per verb table.
//! - LightrError::RefNotFound | InvalidRef  ⇒ exit 2 + one-line stderr `lightr: <msg>`
//! - any other error                         ⇒ exit 1 + one-line stderr `lightr: <msg>`
//! - status dirty                             ⇒ exit 1 (no error message)
//! - run ⇒ pass child's exit code
//! - clap handles usage (already exits 2)
//!
//! R0 exit helpers are kept for backward-compat; R1 handlers use die_internal instead.
#![allow(dead_code)]

use lightr_core::LightrError;

/// Exit with an appropriate code after printing the error to stderr.
pub fn die_from_error(e: &LightrError) -> ! {
    let code = error_exit_code(e);
    eprintln!("lightr: {e}");
    std::process::exit(code);
}

/// Return the exit code for a LightrError without printing.
pub fn error_exit_code(e: &LightrError) -> i32 {
    match e {
        LightrError::RefNotFound(_) | LightrError::InvalidRef(_) => 2,
        _ => 1,
    }
}

/// Exit 0.
pub fn exit_ok() -> ! {
    std::process::exit(0);
}

/// Exit 1 (dirty / budget fail) without a message.
pub fn exit_dirty() -> ! {
    std::process::exit(1);
}

/// Exit with the child's code (run verb).
pub fn exit_child(code: i32) -> ! {
    std::process::exit(code);
}

/// Print error to stderr and return exit code 2 (internal helper for R1 handlers).
/// Print the error and return ITS mapped exit code (R0 contract:
/// RefNotFound|InvalidRef ⇒ 2, everything else ⇒ 1).
pub fn die_lightr(e: &lightr_core::LightrError) -> i32 {
    eprintln!("lightr: {e}");
    error_exit_code(e)
}

/// Usage-class failures only (unknown id, bad grammar): always 2.
pub fn die_internal(e: &dyn std::fmt::Display) -> i32 {
    eprintln!("lightr: {e}");
    2
}

/// Container-resolution failure on a lifecycle verb, mapped to Docker's
/// convention (WP-EXIT-CODE).
///
/// `docker stop|start|restart|kill|... <missing>` prints
/// `Error: No such container: <ref>` and exits **1** — a missing container is
/// NOT a usage error. Lightr's `resolve` distinguishes:
///   - `RefNotFound` ⇒ the container does not exist ⇒ honest
///     `No such container: <ref>` + exit **1** (Docker parity).
///   - `InvalidRef`  ⇒ malformed/ambiguous token (empty ref, ambiguous id
///     prefix) ⇒ a usage/arg-class error ⇒ keep exit **2** with its own
///     message (arg-error codes are unchanged by this WP).
///   - anything else ⇒ exit **1** with its message (an I/O fault while
///     resolving is not a usage error).
///
/// `reference` is the user-supplied token (echoed in the not-found message so
/// the operator sees what they asked for, matching Docker).
pub fn die_resolve(e: &LightrError, reference: &str) -> i32 {
    match e {
        LightrError::RefNotFound(_) => {
            eprintln!("Error: No such container: {reference}");
            1
        }
        LightrError::InvalidRef(_) => {
            eprintln!("lightr: {e}");
            2
        }
        other => {
            eprintln!("lightr: {other}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_not_found_is_exit_2() {
        let e = LightrError::RefNotFound("foo".into());
        assert_eq!(error_exit_code(&e), 2);
    }

    #[test]
    fn invalid_ref_is_exit_2() {
        let e = LightrError::InvalidRef("bad name".into());
        assert_eq!(error_exit_code(&e), 2);
    }

    #[test]
    fn io_error_is_exit_1() {
        let e = LightrError::Io(std::io::Error::other("test"));
        assert_eq!(error_exit_code(&e), 1);
    }

    #[test]
    fn integrity_error_is_exit_1() {
        use lightr_core::Digest;
        let d = Digest([0u8; 32]);
        let e = LightrError::Integrity {
            expected: d,
            actual: d,
        };
        assert_eq!(error_exit_code(&e), 1);
    }

    #[test]
    fn not_found_is_exit_1() {
        use lightr_core::Digest;
        let e = LightrError::NotFound(Digest([0u8; 32]));
        assert_eq!(error_exit_code(&e), 1);
    }

    // ── die_resolve: Docker no-such-container parity (WP-EXIT-CODE) ──────────

    #[test]
    fn die_resolve_ref_not_found_is_exit_1() {
        // A missing container is NOT a usage error — Docker exits 1.
        let e = LightrError::RefNotFound("ghost".into());
        assert_eq!(die_resolve(&e, "ghost"), 1);
    }

    #[test]
    fn die_resolve_invalid_ref_stays_exit_2() {
        // Malformed/ambiguous token is a usage/arg-class error — unchanged at 2.
        let e = LightrError::InvalidRef("ambiguous id prefix: ab".into());
        assert_eq!(die_resolve(&e, "ab"), 2);
    }

    #[test]
    fn die_resolve_io_error_is_exit_1() {
        // An I/O fault while resolving is not a usage error — exit 1.
        let e = LightrError::Io(std::io::Error::other("disk"));
        assert_eq!(die_resolve(&e, "anything"), 1);
    }
}
