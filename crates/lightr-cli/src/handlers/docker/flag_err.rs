//! FIX #74 — the shim's HONEST-error helper for docker flags it cannot honor.
//!
//! The cardinal rule of this shim: never silent-drop a flag. A flag the native
//! side does not parse (or an unrecognized `--flag`) must surface as an explicit
//! error + exit 2, so the user is never misled into thinking `-x` took effect
//! when it was discarded. Both `translate_run` and `translate_build` route
//! unrecognized flags through here for a single, consistent message shape.

/// Print the canonical "unrecognized flag" diagnostic for a docker subcommand
/// and return exit code 2. Callers do `return Err(unsupported_flag(...))`.
pub(super) fn unsupported_flag(subcommand: &str, flag: &str) -> i32 {
    eprintln!("lightr docker: {subcommand}: unrecognized flag '{flag}' — not forwarded (the shim never silently drops a flag)");
    2
}
