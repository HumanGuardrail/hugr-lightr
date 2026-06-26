//! WP-#99 (CRI slice 1): the `__ns-run` re-exec shim dispatch.
//!
//! Mirrors the `__supervise` hidden-subcommand pattern: `lightr-cri-serve` is
//! re-exec'd by the backend as `<current_exe> __ns-run` with a `RunDescriptor`
//! piped on stdin. The REAL logic lives in `lightr_cri_backend::ns_run::run_shim`
//! (it owns the descriptor type + the `lightr-engine` dep), so this is a one-line
//! forward — keeping `lightr-cri-serve` from taking the ns-engine deps directly.

/// Dispatch the `__ns-run` shim. Never returns (exits with the workload's code).
pub fn main() -> ! {
    lightr_cri_backend::ns_run::run_shim()
}
