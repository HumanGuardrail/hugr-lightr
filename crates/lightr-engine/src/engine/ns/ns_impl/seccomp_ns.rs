//! ns-side seccomp compile: turn the (host-visible, pre-pivot) `--seccomp` value
//! into the filter carried to the pre-execv install. Split out of `rootfs.rs` for
//! the <=400-LOC godfile invariant.
//!
//! seccomp is **x86_64-linux-only** (`engine::seccomp` is `#[cfg(target_arch =
//! "x86_64")]`-gated; the filter targets `AUDIT_ARCH_X86_64` and the syscall table
//! uses x86_64 `libc::SYS_*`). On other arches this FAILS CLOSED if a filter is
//! requested — the CLI already refuses `--seccomp` with exit 2, so this is defense
//! in depth in PID 1 (`SeccompFilter` is uninhabited there, so `None` is the only
//! inhabitable value). `unconfined`/`None` ⇒ no filter (same as x86_64 with no flag).

use super::signal::signal_setup_failed;

pub(super) fn compile_seccomp(
    seccomp: Option<&str>,
    exec_ready_fd: Option<libc::c_int>,
) -> Option<super::SeccompFilter> {
    #[cfg(target_arch = "x86_64")]
    return match seccomp {
        Some("default") => match crate::engine::seccomp::compile_default() {
            Ok(c) => Some(c),
            Err(e) => fail(exec_ready_fd, &e.to_string()),
        },
        Some("unconfined") | None => None,
        Some(p) => match crate::engine::seccomp::compile_from_path(p) {
            Ok(c) => Some(c),
            Err(e) => fail(exec_ready_fd, &e.to_string()),
        },
    };
    #[cfg(not(target_arch = "x86_64"))]
    return match seccomp {
        Some("unconfined") | None => None,
        Some(_) => fail(
            exec_ready_fd,
            "x86_64-linux-only (unsupported on this arch)",
        ),
    };
}

/// Signal the exec-ready pipe (so the backend sees an error, not a false `Running`)
/// and `_exit(1)` — never exec unfiltered when a filter was requested/failed.
fn fail(exec_ready_fd: Option<libc::c_int>, msg: &str) -> ! {
    eprintln!("lightr-engine ns: seccomp: {msg}");
    signal_setup_failed(exec_ready_fd, &format!("seccomp: {msg}"));
    unsafe { libc::_exit(1) }
}
