//! WP-#100 (CRI exec slice 1): the `__ns-exec` re-exec shim dispatch.
//!
//! Mirrors `ns_shim` (`__ns-run`): `lightr-cri-serve` is re-exec'd by the backend
//! as `<current_exe> __ns-exec` with an `ExecDescriptor` in `LIGHTR_NSEXEC_DESC`.
//! The REAL logic lives in `lightr_cri_backend::ns_exec::run_exec_shim` (it owns
//! the descriptor type + the raw setns/fork/execve), so this is a one-line
//! forward — keeping `lightr-cri-serve` thin.

/// Dispatch the `__ns-exec` shim. Never returns (exits with the workload's code).
pub fn main() -> ! {
    lightr_cri_backend::ns_exec::run_exec_shim()
}
