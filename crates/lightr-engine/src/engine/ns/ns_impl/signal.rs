//! ns_impl::signal — exec-readiness pipe signalling, the PID-1 reaper loop, and
//! wait-status → exit-code mapping. All items live inside the Linux-gated
//! `ns_impl` module (see `ns/mod.rs`).

pub(super) fn wait_to_exit_code(wstatus: libc::c_int) -> i32 {
    if libc::WIFEXITED(wstatus) {
        libc::WEXITSTATUS(wstatus)
    } else if libc::WIFSIGNALED(wstatus) {
        128 + libc::WTERMSIG(wstatus)
    } else {
        1
    }
}

/// WP-#102: arm the exec-readiness pipe for a SUCCESSFUL exec by setting the
/// write end `FD_CLOEXEC`. A successful `execv` then makes the kernel auto-close
/// it ⇒ the backend's reader sees EOF ⇒ the workload is actually running. Called
/// in the EXECing process immediately before `execv`. No-op when no pipe is wired
/// (`None` ⇒ byte-identical to the pre-#102 path). Best-effort: a failed `fcntl`
/// at worst leaves the fd open so EOF waits for exit — not a false `Running`.
pub(super) fn arm_exec_ready(fd: Option<libc::c_int>) {
    if let Some(fd) = fd {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

/// WP-#102: signal an `execv` FAILURE down the exec-readiness pipe by WRITING the
/// error bytes (the reader distinguishes BYTES ⇒ start failed from EOF ⇒ success).
/// Called AFTER `execv` returns (i.e. it failed) and BEFORE `_exit(127)`. No-op
/// when no pipe is wired. The write is best-effort (raw libc; we are post-fork).
pub(super) fn signal_exec_failed(fd: Option<libc::c_int>, err: &std::io::Error) {
    if let Some(fd) = fd {
        let msg = format!("exec failed: {err}");
        unsafe {
            libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len());
        }
    }
}

/// WP-#104: signal a PID-1 SETUP failure (any pre-execv step: rootfs CString,
/// MS_PRIVATE, bind-mount, put_old, pivot_root, chdir, /dev/shm, RO remount,
/// caps, init-fork) down the exec-readiness pipe by WRITING the message bytes —
/// the SAME bytes-mechanism `signal_exec_failed` uses, so the backend's reader
/// sees BYTES ⇒ start failed (never EOF ⇒ a false `Running` that the reaper then
/// flips to `Exited`). Mirrors `signal_exec_failed` but takes a `&str` (setup
/// failures carry a short context string, not an `io::Error`). Called right
/// BEFORE the corresponding `_exit(1)`. No-op when no pipe is wired (`None` ⇒
/// non-CRI callers are byte-identical to today). Best-effort (raw libc;
/// post-fork). The ONLY no-bytes pipe close stays the SUCCESSFUL execv (CLOEXEC).
pub(super) fn signal_setup_failed(fd: Option<libc::c_int>, msg: &str) {
    if let Some(fd) = fd {
        let line = format!("setup failed: {msg}");
        unsafe {
            libc::write(fd, line.as_ptr() as *const libc::c_void, line.len());
        }
    }
}

/// WP-#95 (`--init`): the minimal PID-1 reaper loop. Blocks in `waitpid(-1)`,
/// reaping every child (orphaned grandchildren re-parent to PID 1). When the
/// tracked `workload_child` exits we record its code, drain any already-exited
/// remaining children (non-blocking — we don't wait on long-lived orphans), then
/// `_exit` with the workload's code so the run's exit status is the workload's.
/// `ECHILD` (no children left) also exits. `EINTR`/other transient errors retry.
/// Raw libc only (post-fork, pre-`_exit`): no allocation in the loop body.
pub(super) fn reaper_loop(workload_child: libc::pid_t) -> ! {
    let mut workload_code: i32 = 0;
    let mut have_code = false;
    loop {
        let mut status: libc::c_int = 0;
        let r = unsafe { libc::waitpid(-1, &mut status, 0) };
        if r == -1 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ECHILD) {
                unsafe { libc::_exit(if have_code { workload_code } else { 0 }) };
            }
            // EINTR or other transient error: retry the wait.
            continue;
        }
        if r == workload_child {
            workload_code = wait_to_exit_code(status);
            have_code = true;
            // Drain any remaining already-exited children (non-blocking), then
            // exit with the workload's code.
            loop {
                let mut st: libc::c_int = 0;
                let w = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG) };
                if w <= 0 {
                    break;
                }
            }
            unsafe { libc::_exit(workload_code) };
        }
        // Some other orphan was reaped — keep looping.
    }
}
