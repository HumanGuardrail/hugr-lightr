//! `lightr bench-compare` handler — the head-to-head benchmark (WP-C).
//!
//! build-spec-parity.md §A0.5 freezes the CLI surface + dispatch; **WP-C fills
//! the body** (the vs-Docker/OrbStack/Apple-container harness). A0 ships an
//! honest stub: it returns an `Unsupported` I/O error and routes it through
//! `die_lightr` (exit 1, `lightr: …` on stderr) — it NEVER prints a fabricated
//! number or fake success. (`lightr-core::LightrError` has no `Unsupported`
//! variant, so the honest closest is `Io(ErrorKind::Unsupported)`.)

use crate::exit::die_lightr;

/// Run the comparison. `vs` = runtimes to compare against, `workload` = which
/// workload(s), `json` = machine-readable output.
pub fn run(vs: &[String], workload: &str, json: bool) -> i32 {
    let _ = (vs, workload, json);
    let e = lightr_core::LightrError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "bench-compare: not yet implemented (WP-C)",
    ));
    die_lightr(&e)
}
