//! WP-#100 (CRI exec slice 1): the `__ns-exec` re-exec shim — the nsenter model.
//!
//! `crictl exec` must ENTER a running `ns` container (setns into its PID-1
//! namespaces), not spawn a host process. `setns` is single-thread-only (it
//! fails with EINVAL on a multithreaded process for user/pid namespaces), and
//! the gRPC serve is multithreaded — so a FRESH, single-threaded exec'd process
//! is MANDATORY. This is the sibling of `ns_run.rs`'s `__ns-run` shim.
//!
//! Like `ns_run`, the shared serialization type ([`ExecDescriptor`]) lives HERE,
//! in `lightr-cri-backend` (the backend builds it; `lightr-cri-serve` only
//! forwards `__ns-exec` to [`run_exec_shim`]), so there is a SINGLE type and no
//! copy-paste drift. Transport is an ENV var (`LIGHTR_NSEXEC_DESC`, JSON) rather
//! than stdin — the backend keeps the child's stdout/stderr piped for capture
//! (exec_sync) or fan-out (open_exec), so stdin stays free; the descriptor
//! carries no secrets and the shim execve's with the container's OWN env so the
//! var never leaks inside.
//!
//! WP-#103 (exec slice 2): tty + interactive stdin. When `desc.tty` is set, the
//! backend wires a pty SLAVE as the shim's stdio (fds 0/1/2), which inherits
//! across the fork to the workload grandchild; just before `execve` the
//! grandchild calls `setsid()` (new session) + `ioctl(TIOCSCTTY)` so the pty
//! slave becomes its controlling terminal INSIDE the container (job control,
//! line editing). Mirrors the host `open_exec_tty` setsid path. The non-tty
//! (pipe) path is unchanged from slice 1.

use serde::{Deserialize, Serialize};

/// Everything the `__ns-exec` shim needs to ENTER one running `ns` container and
/// run a command inside it. The ONE shared type between the backend (builder) and
/// the shim (consumer). Runtime-only values (no memo key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecDescriptor {
    /// Host pid of the container's in-pidns PID 1 (resolved by
    /// `LightrBackend::container_pid1` via cgroup.procs + the NSpid==1 rule). The
    /// shim opens `/proc/<pid1>/ns/{user,net,pid,mnt}` and `setns`es into them.
    pub pid1: u32,
    /// Full argv (program + args). argv[0] is the program; `execve` does NOT do a
    /// PATH search, so it should be an absolute path (slice 1 — matches how the
    /// CRI exec callers pass commands).
    pub argv: Vec<String>,
    /// Working directory inside the container; empty ⇒ `/`.
    pub cwd: String,
    /// Environment as (key, value) pairs — the container's own env, used as the
    /// execve envp so neither `LIGHTR_NSEXEC_DESC` nor the serve's environment
    /// leak into the container.
    pub env: Vec<(String, String)>,
    /// tty requested. When true, the workload grandchild `setsid`s and claims the
    /// pty slave (wired as its stdio by the backend) as its controlling terminal
    /// via `TIOCSCTTY` before `execve` (WP-#103). When false, the non-tty (pipe)
    /// path is taken and this is a no-op in the shim.
    pub tty: bool,
}

/// Entry point for the `__ns-exec` re-exec shim: read an [`ExecDescriptor`] from
/// `LIGHTR_NSEXEC_DESC`, `setns` into the container's namespaces, fork, and
/// `execve` the command inside. NEVER returns. Fail-closed (`_exit(127)`) on any
/// error — it must NEVER fall back to a host exec (that would run OUTSIDE the
/// container = a false result).
pub fn run_exec_shim() -> ! {
    #[cfg(target_os = "linux")]
    {
        run_exec_shim_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("lightr-cri ns-exec: setns-based container exec is linux-only");
        std::process::exit(127)
    }
}

/// Linux implementation — raw libc, single-threaded (the whole point of the
/// re-exec). The order matters and mirrors the WP-#99 engine ordering: open ALL ns
/// fds BEFORE any `setns` (after the mnt swap the host `/proc/<pid1>/ns/*` paths
/// vanish). Join **net FIRST** — the pod netns was created by the HOST (CNI) and is
/// owned by the host INIT user namespace, so we must `setns` into it while still
/// host root; once we enter the container userns we no longer hold CAP_SYS_ADMIN
/// over a host-owned netns (this is the EPERM the first CI run caught). THEN join
/// user (host root may enter a child userns), which grants full caps in it for the
/// remaining joins; THEN pid and mnt — both are owned by the CONTAINER userns
/// (the engine `unshare`d them after NEWUSER). mnt LAST (its swap erases host
/// paths), then `fork` (setns(pid) only moves CHILDREN into the pid ns).
#[cfg(target_os = "linux")]
fn run_exec_shim_linux() -> ! {
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    // 1. Descriptor from the env var (JSON).
    let json = match std::env::var("LIGHTR_NSEXEC_DESC") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lightr-cri ns-exec: LIGHTR_NSEXEC_DESC unset: {e}");
            unsafe { libc::_exit(127) }
        }
    };
    let desc: ExecDescriptor = match serde_json::from_str(&json) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lightr-cri ns-exec: bad descriptor JSON: {e}");
            unsafe { libc::_exit(127) }
        }
    };
    if desc.argv.is_empty() {
        eprintln!("lightr-cri ns-exec: empty argv");
        unsafe { libc::_exit(127) }
    }

    // 2. Open ALL ns fds FIRST (before any setns). Only the four ns.rs
    //    establishes: user+mnt+pid unshared, net joined (no uts/ipc).
    let base = format!("/proc/{}/ns", desc.pid1);
    let open_ns = |name: &str| -> std::fs::File {
        match std::fs::OpenOptions::new()
            .read(true)
            .open(format!("{base}/{name}"))
        {
            Ok(f) => f,
            Err(e) => {
                eprintln!("lightr-cri ns-exec: open {base}/{name}: {e}");
                unsafe { libc::_exit(127) }
            }
        }
    };
    let user_ns = open_ns("user");
    let net_ns = open_ns("net");
    let pid_ns = open_ns("pid");
    let mnt_ns = open_ns("mnt");

    // 3. setns ORDER: net → user → pid → mnt (net while host-root over the
    //    host-owned pod netns; mnt LAST). Fail-closed on any error.
    let do_setns = |f: &std::fs::File, flag: libc::c_int, what: &str| {
        if unsafe { libc::setns(f.as_raw_fd(), flag) } != 0 {
            eprintln!(
                "lightr-cri ns-exec: setns({what}): {}",
                std::io::Error::last_os_error()
            );
            unsafe { libc::_exit(127) }
        }
    };
    do_setns(&net_ns, libc::CLONE_NEWNET, "net"); // FIRST: host-owned pod netns; need host caps
    do_setns(&user_ns, libc::CLONE_NEWUSER, "user"); // then enter the container userns
    do_setns(&pid_ns, libc::CLONE_NEWPID, "pid"); // owned by the container userns
    do_setns(&mnt_ns, libc::CLONE_NEWNS, "mnt"); // LAST (swap erases host paths)

    // 4. fork — setns(CLONE_NEWPID) only moves our CHILD into the pid ns; THIS
    //    process stays put. Parent waitpids the child and relays its status so the
    //    backend reads the real exit code (and reaps it — no zombie).
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        eprintln!(
            "lightr-cri ns-exec: fork: {}",
            std::io::Error::last_os_error()
        );
        unsafe { libc::_exit(127) }
    }
    if pid > 0 {
        let mut status: libc::c_int = 0;
        loop {
            let r = unsafe { libc::waitpid(pid, &mut status, 0) };
            if r < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                eprintln!("lightr-cri ns-exec: waitpid: {err}");
                unsafe { libc::_exit(127) }
            }
            break;
        }
        let code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else if libc::WIFSIGNALED(status) {
            128 + libc::WTERMSIG(status)
        } else {
            127
        };
        unsafe { libc::_exit(code) }
    }

    // 5. Child (now in the container pid ns + user/net/mnt). For a tty exec
    //    (WP-#103): become a session leader and claim the pty slave — already
    //    wired as fds 0/1/2 by the backend (inherited across the fork) — as this
    //    workload's controlling terminal, so job control / line editing work
    //    INSIDE the container. Mirrors the host `open_exec_tty` setsid path, plus
    //    TIOCSCTTY (the new session leader has no ctty until it claims one). Both
    //    are best-effort, matching the host path: setsid can only fail if we were
    //    already a group leader (we are not — fresh fork), and TIOCSCTTY is the
    //    standard ctty claim. The non-tty path leaves stdio (pipes) untouched.
    if desc.tty {
        unsafe {
            libc::setsid();
            libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0);
        }
    }

    // chdir into the workload cwd (fallback /), then execve with the
    // DESCRIPTOR's env.
    let cwd = if desc.cwd.is_empty() {
        "/".to_string()
    } else {
        desc.cwd.clone()
    };
    if let Ok(cwd_c) = CString::new(cwd) {
        if unsafe { libc::chdir(cwd_c.as_ptr()) } != 0 {
            let root = CString::new("/").unwrap();
            unsafe { libc::chdir(root.as_ptr()) };
        }
    }

    let argv_c: Vec<CString> = desc
        .argv
        .iter()
        .map(|a| CString::new(a.as_str()).unwrap_or_default())
        .collect();
    let mut argv_p: Vec<*const libc::c_char> = argv_c.iter().map(|c| c.as_ptr()).collect();
    argv_p.push(std::ptr::null());

    let env_c: Vec<CString> = desc
        .env
        .iter()
        .map(|(k, v)| CString::new(format!("{k}={v}")).unwrap_or_default())
        .collect();
    let mut env_p: Vec<*const libc::c_char> = env_c.iter().map(|c| c.as_ptr()).collect();
    env_p.push(std::ptr::null());

    unsafe { libc::execve(argv_c[0].as_ptr(), argv_p.as_ptr(), env_p.as_ptr()) };
    // execve only returns on error.
    eprintln!(
        "lightr-cri ns-exec: execve {:?}: {}",
        desc.argv.first(),
        std::io::Error::last_os_error()
    );
    unsafe { libc::_exit(127) }
}
