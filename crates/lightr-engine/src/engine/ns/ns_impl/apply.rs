//! ns_impl::apply — the PID-1 pre-execv "apply" steps (ulimits, oom-score-adj,
//! caps, apparmor, seccomp) + the shared apply-and-exec tail. All items live
//! inside the Linux-gated `ns_impl` module (see `ns/mod.rs`).

use super::signal::{arm_exec_ready, signal_exec_failed, signal_setup_failed};
use crate::engine::ns::CAP_LAST_CAP;
use crate::engine::seccomp;
use crate::engine::spec::Ulimit;

/// WP-#95: apply the (already-parsed) capability set in the EXECing process,
/// right before `execv`. `None` ⇒ neither `--cap-*` flag was set ⇒ keep the full
/// userns set (no-op). Called post-fork/pre-exec, so a capset failure must
/// `_exit` (fail-closed) rather than return — exec'ing with the WRONG capability
/// set would be false security (worse than an error).
///
/// WP-#104: caps are the LAST pre-execv step, so a capset failure here must ALSO
/// signal the exec-readiness pipe (`exec_ready_fd`) with bytes before `_exit(1)`
/// — otherwise the kernel-closed fd reads as EOF ⇒ a false `Running`. The fd is
/// threaded in from the (only) two call sites, both in the PID-1 branch. `None`
/// (non-CRI, or no pipe) ⇒ no-op.
/// `--ulimit`: apply each per-process `setrlimit` cap in the EXECing process,
/// EARLY (before caps/user/seccomp). For each entry build `libc::rlimit`
/// (mapping the `u64::MAX` sentinel → `libc::RLIM_INFINITY`) and `setrlimit`.
/// Empty ⇒ no-op (byte-identical to the pre-feature path). Called post-fork, so
/// a failing `setrlimit` is FAIL-CLOSED: it signals the exec-readiness pipe with
/// bytes (so the kernel-closed fd is NOT misread as EOF ⇒ a false `Running`) and
/// `_exit(1)`s rather than exec with the WRONG limits. A rootless hard-limit
/// RAISE beyond the inherited cap EPERMs here — an honest error, never a silent
/// drop. Mirrors `apply_caps_if_any`/`apply_user_if_any`.
pub(super) fn apply_ulimits_if_any(ulimits: &[Ulimit], exec_ready_fd: Option<libc::c_int>) {
    for u in ulimits {
        let to_rlim = |v: u64| -> libc::rlim_t {
            if v == u64::MAX {
                libc::RLIM_INFINITY
            } else {
                v as libc::rlim_t
            }
        };
        let rl = libc::rlimit {
            rlim_cur: to_rlim(u.soft),
            rlim_max: to_rlim(u.hard),
        };
        let r = unsafe { libc::setrlimit(u.resource as _, &rl) };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!(
                "lightr-engine ns: --ulimit setrlimit(resource={}, soft={}, hard={}) failed: {e}",
                u.resource, u.soft, u.hard
            );
            signal_setup_failed(exec_ready_fd, "ulimit setrlimit failed");
            unsafe { libc::_exit(1) };
        }
    }
}

/// `--oom-score-adj`: write the integer to `/proc/self/oom_score_adj` in the
/// EXECing process, EARLY (alongside `apply_ulimits_if_any`, before
/// caps/user/seccomp). Mirrors `lightr-run::apply_cfg::install_oom_score_adj`'s
/// write idiom (open `O_WRONLY` + write the decimal text + close), but here it
/// runs in our own PID (not a `pre_exec` hook) so a plain heap-formatted string
/// is fine. `None` ⇒ no-op (byte-identical to the pre-feature path). Called
/// post-fork, so a failing write is FAIL-CLOSED: a rootless RAISE always works,
/// but a LOWERING below the parent's score EPERMs — an honest error, never a
/// silent drop. It signals the exec-readiness pipe with bytes (so the
/// kernel-closed fd is NOT misread as EOF ⇒ a false `Running`) and `_exit(1)`s
/// rather than exec with the WRONG score. Mirrors `apply_ulimits_if_any`.
pub(super) fn apply_oom_score_adj_if_any(oom: Option<i32>, exec_ready_fd: Option<libc::c_int>) {
    let adj = match oom {
        None => return,
        Some(a) => a,
    };
    let s = format!("{adj}");
    let path = c"/proc/self/oom_score_adj";
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        eprintln!("lightr-engine ns: --oom-score-adj open(/proc/self/oom_score_adj) failed: {e}");
        signal_setup_failed(exec_ready_fd, "oom_score_adj open failed");
        unsafe { libc::_exit(1) };
    }
    let n = unsafe { libc::write(fd, s.as_ptr() as *const libc::c_void, s.len()) };
    let werr = if n < 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    unsafe { libc::close(fd) };
    if let Some(e) = werr {
        eprintln!("lightr-engine ns: --oom-score-adj write({adj}) failed: {e}");
        signal_setup_failed(exec_ready_fd, "oom_score_adj write failed");
        unsafe { libc::_exit(1) };
    }
}

pub(super) fn apply_caps_if_any(desired: Option<&[u32]>, exec_ready_fd: Option<libc::c_int>) {
    if let Some(d) = desired {
        if let Err(e) = apply_caps(d) {
            eprintln!("lightr-engine ns: capability enforcement failed: {e}");
            signal_setup_failed(exec_ready_fd, "capability enforcement failed"); // WP-#104
            unsafe { libc::_exit(1) };
        }
    }
}

/// WP-#106: apply an AppArmor profile to the EXECing process via the kernel's
/// `aa_change_onexec` mechanism — write `exec <profile>` to the apparmor exec
/// attr right before `execv`, so the kernel transitions the new image into the
/// profile on the exec (the standard runc/crun method). `None` ⇒ no change
/// (inherit). Called post-fork/pre-exec, AFTER caps, so a failure must `_exit`
/// (fail-closed) — exec'ing UNCONFINED when a profile was requested would be
/// false security (worse than an error), and it is what makes the critest
/// "should error on unloadable profile" pass.
///
/// Like `apply_caps_if_any`, an apply failure ALSO signals the exec-readiness
/// pipe (`exec_ready_fd`) with bytes before `_exit(1)` — otherwise the
/// kernel-closed fd reads as EOF ⇒ a false `Running`. `None` (no pipe) ⇒ no-op.
pub(super) fn apply_apparmor_if_any(profile: Option<&str>, exec_ready_fd: Option<libc::c_int>) {
    if let Some(profile) = profile {
        if let Err(e) = apply_apparmor(profile) {
            eprintln!("lightr-engine ns: apparmor: {e}");
            signal_setup_failed(exec_ready_fd, &format!("apparmor: {e}"));
            unsafe { libc::_exit(1) };
        }
    }
}

/// WP-#108: install a (pre-compiled) seccomp cBPF filter in the EXECing process,
/// right before `execv` and AFTER the apparmor apply. The profile was COMPILED
/// EARLY (pre-pivot, while the host path was visible); this LATE step only
/// installs it (NO_NEW_PRIVS + `seccomp(2)`/`prctl`). `None` ⇒ no profile or
/// `"unconfined"` ⇒ explicit no-op. Like `apply_caps_if_any`/`apply_apparmor_if_any`,
/// an install failure is fail-closed: it signals the exec-readiness pipe with
/// bytes (so the kernel-closed fd is NOT misread as EOF ⇒ a false `Running`) and
/// `_exit(1)`s rather than exec UNFILTERED when a filter was requested.
pub(super) fn apply_seccomp_if_any(
    compiled: Option<&seccomp::CompiledSeccomp>,
    exec_ready_fd: Option<libc::c_int>,
) {
    if let Some(c) = compiled {
        if let Err(e) = c.apply() {
            eprintln!("lightr-engine ns: seccomp: {e}");
            signal_setup_failed(exec_ready_fd, &format!("seccomp: {e}"));
            unsafe { libc::_exit(1) };
        }
    }
}

/// WP-#106: write `exec <profile>` to the AppArmor exec attr (aa_change_onexec
/// wire format). Newer kernels expose the per-LSM path
/// `/proc/self/attr/apparmor/exec`; older kernels only have
/// `/proc/self/attr/exec` — so an `ENOENT` on the former falls back to the
/// latter. For `"unconfined"` the kernel accepts `exec unconfined`. Any open OR
/// write error (profile not loaded / not permitted) is returned so the caller
/// fails closed. Uses std file I/O — consistent with the other PID-1 setup steps
/// here (`create_dir_all`, `symlink`); the child is single-threaded post-fork.
fn apply_apparmor(profile: &str) -> std::io::Result<()> {
    use std::io::Write;
    let cmd = format!("exec {profile}");
    let mut f = match std::fs::OpenOptions::new()
        .write(true)
        .open("/proc/self/attr/apparmor/exec")
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::OpenOptions::new()
            .write(true)
            .open("/proc/self/attr/exec")?,
        Err(e) => return Err(e),
    };
    f.write_all(cmd.as_bytes())?;
    Ok(())
}

/// WP-#94: enforce the `desired` capability set (numbers, sorted) via raw libc.
///
/// Two complementary steps, in this order:
///   1. **Bounding set** — `prctl(PR_CAPBSET_DROP, cap)` for every cap NOT in
///      `desired`. This is irreversible: it prevents the process (and its exec'd
///      children) from ever RE-acquiring the cap, even via a setuid/file-cap
///      binary. A cap beyond this kernel's `CAP_LAST_CAP` returns `EINVAL` —
///      treated as "already absent" (not fatal); any other error is fail-closed.
///   2. **capset (v3 ABI)** — set permitted = effective = inheritable = the
///      desired set. Dropping a cap from `permitted` also strips it from
///      `effective`, so together with the bounding-set drop the cap is gone for
///      good. We do NOT change uids here, so no `PR_SET_KEEPCAPS` is needed; the
///      mapped-root process keeps its caps through `execv` via permitted/effective.
///      (Ambient caps are NOT set — a `--cap-add` for a non-root `--user` would
///      additionally need ambient caps; documented refinement, out of scope.)
///
/// The two 32-bit data words cover caps 0..31 (word 0) and 32..63 (word 1):
/// bit `(cap % 32)` in word `(cap / 32)`.
fn apply_caps(desired: &[u32]) -> std::io::Result<()> {
    use std::collections::BTreeSet;
    let want: BTreeSet<u32> = desired.iter().copied().collect();

    // 1. Drop every cap NOT desired from the bounding set.
    for cap in 0..=CAP_LAST_CAP {
        if want.contains(&cap) {
            continue;
        }
        let r = unsafe {
            libc::prctl(
                libc::PR_CAPBSET_DROP,
                cap as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            // A cap number beyond this kernel's CAP_LAST_CAP ⇒ EINVAL; it is
            // already absent, so this is benign (robust against older kernels).
            if e.raw_os_error() == Some(libc::EINVAL) {
                continue;
            }
            return Err(e);
        }
    }

    // 2. capset (version 3): permitted = effective = inheritable = desired.
    #[repr(C)]
    struct CapUserHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CapUserData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    // _LINUX_CAPABILITY_VERSION_3 — the only ABI that addresses caps 32..63.
    const CAP_VERSION_3: u32 = 0x2008_0522;
    let hdr = CapUserHeader {
        version: CAP_VERSION_3,
        pid: 0, // 0 = the calling thread (self).
    };
    let mut data = [CapUserData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    for &cap in &want {
        let word = (cap / 32) as usize;
        let bit = 1u32 << (cap % 32);
        data[word].effective |= bit;
        data[word].permitted |= bit;
        data[word].inheritable |= bit;
    }
    let r = unsafe { libc::syscall(libc::SYS_capset, &hdr, data.as_ptr()) };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// PID-1 pre-exec step, extracted verbatim from `run_in_namespaces`: chdir into
/// the cwd-within-rootfs (fallback `/`), then execvp-style PATH-resolve argv[0]
/// against the CONTAINER rootfs (post-pivot, so `access(X_OK)` hits the container)
/// using the workload's own PATH. Runs in PID 1 (BEFORE any `--init` fork), so the
/// resolved CString is copied across that fork and stays valid for the workload's
/// execv. Fail-closed: an unresolvable argv[0] signals the exec-readiness pipe and
/// `_exit(127)`s (byte-identical to the pre-extraction path). Returns the resolved
/// program CString.
pub(super) fn chdir_and_resolve(
    cwd: &str,
    command: &[String],
    env_path: Option<&str>,
    exec_ready_fd: Option<libc::c_int>,
) -> std::ffi::CString {
    // chdir to cwd-within-rootfs, or fallback to /
    let cwd_in = if cwd.is_empty() { "/" } else { cwd };
    let cwd_c = match std::ffi::CString::new(cwd_in.as_bytes()) {
        Ok(c) => c,
        Err(_) => std::ffi::CString::new("/").unwrap(),
    };
    unsafe {
        libc::chdir(cwd_c.as_ptr());
    }

    // CRITEST "starting container": execvp-style PATH resolution of argv[0].
    // critest starts containers with a BARE command (`top`); raw `execv` does
    // NO PATH search, so `execv("top")` ENOENTs (the 20/34 failure root cause).
    // Resolve HERE — post-pivot, so `access(X_OK)` checks hit the CONTAINER
    // rootfs (not the host) — against the workload's own PATH (`env_path`, or
    // the standard default when absent). A path-qualified argv[0] (contains a
    // `/`) is returned as-is (NO search) ⇒ byte-identical to the pre-fix path.
    // Fail-closed: if nothing resolves, signal the exec-readiness pipe and
    // `_exit(127)` exactly like the existing execv-ENOENT path — never exec an
    // empty/wrong program. The resolved CString is copied across the `--init`
    // fork below, so it stays valid for the workload's execv.
    match crate::pathres::resolve_in_path(&command[0], env_path) {
        Some(p) => p,
        None => {
            let e = std::io::Error::from_raw_os_error(libc::ENOENT);
            signal_exec_failed(exec_ready_fd, &e);
            eprintln!(
                "lightr-engine ns: exec failed: {:?} not found in container PATH",
                command[0]
            );
            unsafe { libc::_exit(127) };
        }
    }
}

/// The shared PID-1 apply-and-exec tail, extracted verbatim from the two
/// (byte-identical) arms of `run_in_namespaces`'s `if init { … } else { … }`
/// block. Runs the EARLY ulimits/oom-score steps, then caps → apparmor → user →
/// seccomp (the fixed order — caps/apparmor need privilege; the `--user` drop
/// must precede any seccomp filter; seccomp is armed last), arms the exec-ready
/// pipe (CLOEXEC), then `execv`s the PATH-resolved program. Every step is
/// fail-closed via `_exit` inside its `apply_*` helper; if `execv` itself returns
/// it FAILED, so we signal the pipe and `_exit(127)`. Never returns (`-> !`).
/// The CStrings backing `prog_resolved`/`argv_ptrs` are owned by the caller and
/// outlive this call (it always execs or `_exit`s).
pub(super) fn apply_and_exec(
    desired_caps: Option<&[u32]>,
    apparmor: Option<&str>,
    user: Option<&str>,
    use_range: bool,
    compiled_seccomp: Option<&seccomp::CompiledSeccomp>,
    ulimits: &[Ulimit],
    oom_score_adj: Option<i32>,
    exec_ready_fd: Option<libc::c_int>,
    prog_resolved: &std::ffi::CStr,
    argv_ptrs: &[*const libc::c_char],
) -> ! {
    // `--ulimit`: apply per-process `setrlimit` caps EARLY — BEFORE caps/user/
    // seccomp, so a hard-limit RAISE still holds CAP_SYS_RESOURCE (a lowering
    // always works). Fail-closed.
    apply_ulimits_if_any(ulimits, exec_ready_fd);
    // `--oom-score-adj`: write /proc/self/oom_score_adj EARLY (a rootless RAISE
    // always works; a LOWERING below the parent EPERMs ⇒ fail-closed). `None` ⇒
    // no-op.
    apply_oom_score_adj_if_any(oom_score_adj, exec_ready_fd);
    // Caps applied LAST, in the execing process (fail-closed: a capset failure
    // `_exit`s rather than exec with the WRONG set).
    apply_caps_if_any(desired_caps, exec_ready_fd);
    // WP-#106: apply the AppArmor profile LAST (after caps), right before execv
    // (aa_change_onexec). Fail-closed: a profile that can't be applied `_exit`s
    // rather than exec unconfined.
    apply_apparmor_if_any(apparmor, exec_ready_fd);
    // `--user`: drop to the target uid/gid AFTER caps/apparmor (which need
    // privilege) but BEFORE seccomp — the switch must run while we still hold
    // CAP_SETUID/SETGID and before any filter could block the setuid syscalls.
    // `execve` then naturally clears caps for the now non-root process.
    // Fail-closed. `None` ⇒ no-op.
    super::user::apply_user_if_any(user, exec_ready_fd, use_range);
    // WP-#108: install the (pre-compiled) seccomp filter LAST — after apparmor,
    // right before execv. Fail-closed: an install failure `_exit`s rather than
    // exec unfiltered. `None` (no profile / "unconfined") ⇒ no-op.
    apply_seccomp_if_any(compiled_seccomp, exec_ready_fd);
    // WP-#102: arm the exec-success pipe (CLOEXEC) right before execv — a
    // successful execv auto-closes it ⇒ the backend's reader sees EOF.
    arm_exec_ready(exec_ready_fd);
    // PATH-resolved program (argv unchanged ⇒ argv[0] stays the conventional
    // name, matching execvp).
    unsafe { libc::execv(prog_resolved.as_ptr(), argv_ptrs.as_ptr()) };
    // execv returned ⇒ it FAILED. Signal BYTES down the pipe first.
    let e = std::io::Error::last_os_error();
    signal_exec_failed(exec_ready_fd, &e);
    eprintln!("lightr-engine ns: exec failed: {e}");
    unsafe { libc::_exit(127) };
}
