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
}
