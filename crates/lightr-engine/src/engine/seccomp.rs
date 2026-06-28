//! WP-#108 (seccomp): hand-rolled OCI seccomp profile → classic-BPF (cBPF)
//! compiler + apply, for the rootless `ns` engine. ZERO new crate dependencies
//! (no `seccompiler`, no `libseccomp`) — just `serde_json` (already a workspace
//! dep) for the profile parse and raw `libc` for the install.
//!
//! Scope (FROZEN, fail-closed): only the simple, common OCI shapes are supported.
//! Anything outside the supported set returns an `io::Error` so the caller
//! (PID 1, pre-execv) `_exit`s rather than exec under a WRONG/absent filter —
//! the same fail-closed discipline as #106 AppArmor.
//!
//! SUPPORTED:
//!   * `defaultAction` ∈ {ALLOW, ERRNO, KILL, KILL_PROCESS, KILL_THREAD}.
//!   * Per-syscall entries with NO `args` (arg-conditioned rules UNSUPPORTED).
//!   * ALL syscall entries share ONE action (mixed per-syscall actions
//!     UNSUPPORTED) — that action ∈ the same set as `defaultAction`.
//!   * Syscall NAMEs resolved → numbers via a `libc::SYS_*` table (so the numbers
//!     are target-correct). An unknown name FAILS CLOSED.
//!
//! The compiled filter is a flat two-ret-block cBPF program:
//!   load arch → (foreign arch ⇒ default-action) → load nr → for each listed
//!   syscall JEQ→listed-action ret, else fall through to the default-action ret.

#![cfg(target_os = "linux")]

use std::io::{Error, ErrorKind, Read};

// ── seccomp_data field offsets (uapi/linux/seccomp.h `struct seccomp_data`) ──────
const SECCOMP_DATA_NR_OFFSET: u32 = 0; // u32 nr
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4; // u32 arch

// ── audit arch (uapi/linux/audit.h). x86_64 only (this engine's validated arch). ─
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;

// ── seccomp return actions (uapi/linux/seccomp.h) ───────────────────────────────
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_KILL_THREAD: u32 = 0x0000_0000; // a.k.a. SECCOMP_RET_KILL
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000; // | (errno & 0xffff)
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

// ── classic-BPF opcodes (uapi/linux/bpf_common.h) ───────────────────────────────
const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

// ── seccomp(2) / prctl seccomp constants (uapi/linux/seccomp.h, sys/prctl.h) ─────
const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
const SECCOMP_MODE_FILTER: libc::c_int = 2;

// ── OCI seccomp profile JSON (the runtime-spec subset we accept) ─────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciSeccomp {
    default_action: String,
    #[serde(default)]
    #[allow(dead_code)] // parsed for completeness; we always compile for x86_64.
    architectures: Vec<String>,
    #[serde(default)]
    syscalls: Vec<OciSyscall>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciSyscall {
    names: Vec<String>,
    action: String,
    #[serde(default)]
    #[allow(dead_code)] // accepted shape carries no errno per-entry use beyond `action`.
    errno_ret: Option<u32>,
    /// Arg-conditioned rules are UNSUPPORTED — a non-empty `args` FAILS CLOSED.
    #[serde(default)]
    args: Vec<serde_json::Value>,
}

/// Map an OCI `SCMP_ACT_*` string + optional errnoRet to a `SECCOMP_RET_*` value.
/// Unknown action ⇒ `None` (caller fails closed).
fn action_ret(action: &str, errno_ret: Option<u32>) -> Option<u32> {
    Some(match action {
        "SCMP_ACT_ALLOW" => SECCOMP_RET_ALLOW,
        "SCMP_ACT_ERRNO" => {
            // errno = low 16 bits; default 1 (EPERM) when unspecified.
            let errno = errno_ret.unwrap_or(1) & 0xffff;
            SECCOMP_RET_ERRNO | errno
        }
        "SCMP_ACT_KILL" | "SCMP_ACT_KILL_THREAD" => SECCOMP_RET_KILL_THREAD,
        "SCMP_ACT_KILL_PROCESS" => SECCOMP_RET_KILL_PROCESS,
        _ => return None,
    })
}

/// A compiled, ready-to-install classic-BPF seccomp filter.
pub struct CompiledSeccomp {
    prog: Vec<libc::sock_filter>,
}

/// Read + parse + compile an OCI seccomp JSON profile at `path` into a cBPF
/// program. Fails closed (`io::Error`) on any unsupported shape so the caller
/// never execs under a wrong/absent filter.
pub fn compile_from_path(path: &str) -> std::io::Result<CompiledSeccomp> {
    let mut buf = String::new();
    std::fs::File::open(path)?.read_to_string(&mut buf)?;
    let profile: OciSeccomp = serde_json::from_str(&buf)
        .map_err(|e| Error::new(ErrorKind::InvalidData, format!("seccomp profile parse: {e}")))?;
    compile(&profile)
}

/// Compile the BUILT-IN `--seccomp default` curated profile (vendored
/// `seccomp_default.json`): a default-deny (ERRNO/EPERM) allow-list derived from
/// the Docker/moby default profile, filtered to the x86_64 names `syscall_nr`
/// resolves. Same fail-closed `compile` path as `compile_from_path`, but the
/// profile is embedded at build time (`include_str!`) so it needs no host file.
pub fn compile_default() -> std::io::Result<CompiledSeccomp> {
    const DEFAULT_PROFILE: &str = include_str!("seccomp_default.json");
    let profile: OciSeccomp = serde_json::from_str(DEFAULT_PROFILE).map_err(|e| {
        Error::new(
            ErrorKind::InvalidData,
            format!("built-in seccomp profile parse: {e}"),
        )
    })?;
    compile(&profile)
}

fn err_unsupported(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidData, msg.into())
}

fn compile(profile: &OciSeccomp) -> std::io::Result<CompiledSeccomp> {
    // Default action — must be a supported shape.
    let default_ret = action_ret(&profile.default_action, None).ok_or_else(|| {
        err_unsupported(format!(
            "unsupported seccomp defaultAction: {}",
            profile.default_action
        ))
    })?;

    // Collect every listed syscall NUMBER. All entries must share ONE action
    // (mixed per-syscall actions UNSUPPORTED) and carry NO `args`.
    let mut listed_ret: Option<u32> = None;
    let mut nrs: Vec<i64> = Vec::new();
    for sc in &profile.syscalls {
        if !sc.args.is_empty() {
            return Err(err_unsupported(
                "arg-conditioned seccomp rules are unsupported (syscall entry has `args`)",
            ));
        }
        let ret = action_ret(&sc.action, sc.errno_ret).ok_or_else(|| {
            err_unsupported(format!("unsupported seccomp syscall action: {}", sc.action))
        })?;
        match listed_ret {
            None => listed_ret = Some(ret),
            Some(prev) if prev != ret => {
                return Err(err_unsupported(
                    "mixed per-syscall seccomp actions are unsupported (all entries must share one action)",
                ));
            }
            Some(_) => {}
        }
        for name in &sc.names {
            let nr = syscall_nr(name).ok_or_else(|| {
                err_unsupported(format!("unsupported syscall in seccomp profile: {name}"))
            })?;
            nrs.push(nr);
        }
    }

    // No listed syscalls ⇒ a degenerate "default only" filter (still valid).
    let listed_ret = listed_ret.unwrap_or(default_ret);

    // ── Build the flat cBPF program ──────────────────────────────────────────
    // Layout (the DEFAULT-ret MUST come first — it is the FALL-THROUGH target):
    //   [0] LD  arch
    //   [1] JEQ arch == X86_64 ? -> [2] (load nr) : -> default-ret
    //   [2] LD  nr
    //   [3..3+N] JEQ nr == nrs[i] ? -> JUMP to listed-ret : fall through
    //   [3+N]    RET default-ret   <- non-matching syscalls FALL THROUGH to here
    //   [3+N+1]  RET listed-ret    <- matching JEQs JUMP past default to here
    // All jt/jf are RELATIVE offsets counted from the instruction AFTER the jump.
    //
    // ORDER IS LOAD-BEARING: a flat JEQ chain falls through on no-match, so the
    // block that immediately follows the JEQs is the no-match action — that MUST
    // be the DEFAULT action. (WP-#108 first cut had these two ret blocks swapped,
    // so EVERY non-listed syscall hit the listed ERRNO ⇒ the filter denied all
    // syscalls ⇒ musl segfaulted. The `run_bpf` simulator tests below pin this.)
    let n = nrs.len();
    let default_ret_idx = 3 + n; // 0:ld arch, 1:jeq arch, 2:ld nr, then N jeqs
    let listed_ret_idx = default_ret_idx + 1;

    let mut prog: Vec<libc::sock_filter> = Vec::with_capacity(listed_ret_idx + 1);

    // [0] LD arch
    prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARCH_OFFSET));
    // [1] JEQ arch == X86_64 → continue (jt=0, next is LD nr); foreign arch →
    // default ret. jf is the distance from the instruction AFTER this jump
    // (index 2) to the default-ret block.
    let jf_to_default = (default_ret_idx - 2) as u8;
    prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 0, jf_to_default));
    // [2] LD nr
    prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFFSET));
    // [3..] one JEQ per listed syscall: match → JUMP to listed-ret; no match →
    // fall through (eventually to the default-ret that sits right after the chain).
    for (i, nr) in nrs.iter().enumerate() {
        let here = 3 + i; // this instruction's index
        // distance from the instruction AFTER this jump to the listed-ret block.
        let jt = (listed_ret_idx - (here + 1)) as u8;
        prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *nr as u32, jt, 0));
    }
    // default-ret block FIRST (the fall-through target for non-matching syscalls)
    prog.push(stmt(BPF_RET | BPF_K, default_ret));
    // listed-ret block (the JUMP target for matched syscalls)
    prog.push(stmt(BPF_RET | BPF_K, listed_ret));

    Ok(CompiledSeccomp { prog })
}

#[inline]
fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

#[inline]
fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

impl CompiledSeccomp {
    /// Install the compiled filter on the CURRENT thread/process. Sets
    /// `NO_NEW_PRIVS` first (required for an unprivileged seccomp filter), then
    /// installs via `seccomp(2)` (preferred), falling back to
    /// `prctl(PR_SET_SECCOMP)` on an `ENOSYS` kernel. Returns `Err` on any
    /// failure so the caller fails closed.
    pub fn apply(&self) -> std::io::Result<()> {
        // 1. NO_NEW_PRIVS — without it the kernel rejects an unprivileged filter.
        let r = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if r != 0 {
            return Err(Error::last_os_error());
        }

        let fprog = libc::sock_fprog {
            len: self.prog.len() as u16,
            filter: self.prog.as_ptr() as *mut libc::sock_filter,
        };

        // 2. Install via seccomp(2); fall back to prctl on ENOSYS.
        let r = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                SECCOMP_SET_MODE_FILTER,
                0u64,
                &fprog as *const _,
            )
        };
        if r == 0 {
            return Ok(());
        }
        let e = Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENOSYS) {
            let r = unsafe {
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    SECCOMP_MODE_FILTER,
                    &fprog as *const _,
                    0,
                    0,
                )
            };
            if r == 0 {
                return Ok(());
            }
            return Err(Error::last_os_error());
        }
        Err(e)
    }
}

/// Resolve a Linux syscall NAME → its x86_64 number via the `libc::SYS_*`
/// constants (so the numbers are target-correct). Covers the full set named by
/// the Docker default seccomp profile plus the common remainder; an unknown name
/// returns `None` so the caller fails closed. The `libc::SYS_*` constants are
/// `i64`, returned as such for the BPF `k` cast.
fn syscall_nr(name: &str) -> Option<i64> {
    Some(match name {
        "accept" => libc::SYS_accept,
        "accept4" => libc::SYS_accept4,
        "access" => libc::SYS_access,
        "adjtimex" => libc::SYS_adjtimex,
        "alarm" => libc::SYS_alarm,
        // x86_64 nr 158 — glibc/musl TLS setup calls this in every program's
        // startup (ARCH_SET_FS); REQUIRED for the `default` allow-list or the C
        // runtime traps before `main`. Added for the built-in profile (WP-#111+).
        "arch_prctl" => libc::SYS_arch_prctl,
        "bind" => libc::SYS_bind,
        "brk" => libc::SYS_brk,
        "capget" => libc::SYS_capget,
        "capset" => libc::SYS_capset,
        "chdir" => libc::SYS_chdir,
        "chmod" => libc::SYS_chmod,
        "chown" => libc::SYS_chown,
        "chroot" => libc::SYS_chroot,
        "clock_adjtime" => libc::SYS_clock_adjtime,
        "clock_getres" => libc::SYS_clock_getres,
        "clock_gettime" => libc::SYS_clock_gettime,
        "clock_nanosleep" => libc::SYS_clock_nanosleep,
        "clone" => libc::SYS_clone,
        "clone3" => libc::SYS_clone3,
        "close" => libc::SYS_close,
        "close_range" => libc::SYS_close_range,
        "connect" => libc::SYS_connect,
        "copy_file_range" => libc::SYS_copy_file_range,
        "creat" => libc::SYS_creat,
        "dup" => libc::SYS_dup,
        "dup2" => libc::SYS_dup2,
        "dup3" => libc::SYS_dup3,
        "epoll_create" => libc::SYS_epoll_create,
        "epoll_create1" => libc::SYS_epoll_create1,
        "epoll_ctl" => libc::SYS_epoll_ctl,
        "epoll_ctl_old" => libc::SYS_epoll_ctl_old,
        "epoll_pwait" => libc::SYS_epoll_pwait,
        "epoll_pwait2" => libc::SYS_epoll_pwait2,
        "epoll_wait" => libc::SYS_epoll_wait,
        "epoll_wait_old" => libc::SYS_epoll_wait_old,
        "eventfd" => libc::SYS_eventfd,
        "eventfd2" => libc::SYS_eventfd2,
        "execve" => libc::SYS_execve,
        "execveat" => libc::SYS_execveat,
        "exit" => libc::SYS_exit,
        "exit_group" => libc::SYS_exit_group,
        "faccessat" => libc::SYS_faccessat,
        "faccessat2" => libc::SYS_faccessat2,
        "fadvise64" => libc::SYS_fadvise64,
        "fallocate" => libc::SYS_fallocate,
        "fanotify_mark" => libc::SYS_fanotify_mark,
        "fchdir" => libc::SYS_fchdir,
        "fchmod" => libc::SYS_fchmod,
        "fchmodat" => libc::SYS_fchmodat,
        "fchown" => libc::SYS_fchown,
        "fchownat" => libc::SYS_fchownat,
        "fcntl" => libc::SYS_fcntl,
        "fdatasync" => libc::SYS_fdatasync,
        "fgetxattr" => libc::SYS_fgetxattr,
        "flistxattr" => libc::SYS_flistxattr,
        "flock" => libc::SYS_flock,
        "fork" => libc::SYS_fork,
        "fremovexattr" => libc::SYS_fremovexattr,
        "fsetxattr" => libc::SYS_fsetxattr,
        "fstat" => libc::SYS_fstat,
        "fstatfs" => libc::SYS_fstatfs,
        "fsync" => libc::SYS_fsync,
        "ftruncate" => libc::SYS_ftruncate,
        "futex" => libc::SYS_futex,
        "futex_waitv" => libc::SYS_futex_waitv,
        "futimesat" => libc::SYS_futimesat,
        "getcpu" => libc::SYS_getcpu,
        "getcwd" => libc::SYS_getcwd,
        "getdents" => libc::SYS_getdents,
        "getdents64" => libc::SYS_getdents64,
        "getegid" => libc::SYS_getegid,
        "geteuid" => libc::SYS_geteuid,
        "getgid" => libc::SYS_getgid,
        "getgroups" => libc::SYS_getgroups,
        "getitimer" => libc::SYS_getitimer,
        "getpeername" => libc::SYS_getpeername,
        "getpgid" => libc::SYS_getpgid,
        "getpgrp" => libc::SYS_getpgrp,
        "getpid" => libc::SYS_getpid,
        "getppid" => libc::SYS_getppid,
        "getpriority" => libc::SYS_getpriority,
        "get_robust_list" => libc::SYS_get_robust_list,
        "getrandom" => libc::SYS_getrandom,
        "getresgid" => libc::SYS_getresgid,
        "getresuid" => libc::SYS_getresuid,
        "getrlimit" => libc::SYS_getrlimit,
        "getrusage" => libc::SYS_getrusage,
        "getsid" => libc::SYS_getsid,
        "getsockname" => libc::SYS_getsockname,
        "getsockopt" => libc::SYS_getsockopt,
        "gettid" => libc::SYS_gettid,
        "gettimeofday" => libc::SYS_gettimeofday,
        "getuid" => libc::SYS_getuid,
        "getxattr" => libc::SYS_getxattr,
        "inotify_add_watch" => libc::SYS_inotify_add_watch,
        "inotify_init" => libc::SYS_inotify_init,
        "inotify_init1" => libc::SYS_inotify_init1,
        "inotify_rm_watch" => libc::SYS_inotify_rm_watch,
        "io_cancel" => libc::SYS_io_cancel,
        "io_destroy" => libc::SYS_io_destroy,
        "io_getevents" => libc::SYS_io_getevents,
        // NOTE: `io_pgetevents` (x86_64 nr 333) is absent from the pinned libc
        // (0.2.186) `SYS_*` set, so it is intentionally omitted — an unknown name
        // fails closed (the safety net), which is the correct conservative behavior
        // for a profile that lists it (it is not in the Docker default's hot path).
        "io_setup" => libc::SYS_io_setup,
        "io_submit" => libc::SYS_io_submit,
        "io_uring_enter" => libc::SYS_io_uring_enter,
        "io_uring_register" => libc::SYS_io_uring_register,
        "io_uring_setup" => libc::SYS_io_uring_setup,
        "ioctl" => libc::SYS_ioctl,
        "ioprio_get" => libc::SYS_ioprio_get,
        "ioprio_set" => libc::SYS_ioprio_set,
        "kcmp" => libc::SYS_kcmp,
        "keyctl" => libc::SYS_keyctl,
        "kill" => libc::SYS_kill,
        "landlock_add_rule" => libc::SYS_landlock_add_rule,
        "landlock_create_ruleset" => libc::SYS_landlock_create_ruleset,
        "landlock_restrict_self" => libc::SYS_landlock_restrict_self,
        "lchown" => libc::SYS_lchown,
        "lgetxattr" => libc::SYS_lgetxattr,
        "link" => libc::SYS_link,
        "linkat" => libc::SYS_linkat,
        "listen" => libc::SYS_listen,
        "listxattr" => libc::SYS_listxattr,
        "llistxattr" => libc::SYS_llistxattr,
        "lremovexattr" => libc::SYS_lremovexattr,
        "lseek" => libc::SYS_lseek,
        "lsetxattr" => libc::SYS_lsetxattr,
        "lstat" => libc::SYS_lstat,
        "madvise" => libc::SYS_madvise,
        "mbind" => libc::SYS_mbind,
        "membarrier" => libc::SYS_membarrier,
        "memfd_create" => libc::SYS_memfd_create,
        "mincore" => libc::SYS_mincore,
        "mkdir" => libc::SYS_mkdir,
        "mkdirat" => libc::SYS_mkdirat,
        "mknod" => libc::SYS_mknod,
        "mknodat" => libc::SYS_mknodat,
        "mlock" => libc::SYS_mlock,
        "mlock2" => libc::SYS_mlock2,
        "mlockall" => libc::SYS_mlockall,
        "mmap" => libc::SYS_mmap,
        "mount" => libc::SYS_mount,
        "move_mount" => libc::SYS_move_mount,
        "mprotect" => libc::SYS_mprotect,
        "mq_getsetattr" => libc::SYS_mq_getsetattr,
        "mq_notify" => libc::SYS_mq_notify,
        "mq_open" => libc::SYS_mq_open,
        "mq_timedreceive" => libc::SYS_mq_timedreceive,
        "mq_timedsend" => libc::SYS_mq_timedsend,
        "mq_unlink" => libc::SYS_mq_unlink,
        "mremap" => libc::SYS_mremap,
        "msgctl" => libc::SYS_msgctl,
        "msgget" => libc::SYS_msgget,
        "msgrcv" => libc::SYS_msgrcv,
        "msgsnd" => libc::SYS_msgsnd,
        "msync" => libc::SYS_msync,
        "munlock" => libc::SYS_munlock,
        "munlockall" => libc::SYS_munlockall,
        "munmap" => libc::SYS_munmap,
        "name_to_handle_at" => libc::SYS_name_to_handle_at,
        "nanosleep" => libc::SYS_nanosleep,
        "newfstatat" => libc::SYS_newfstatat,
        "open" => libc::SYS_open,
        "openat" => libc::SYS_openat,
        "openat2" => libc::SYS_openat2,
        "pause" => libc::SYS_pause,
        "personality" => libc::SYS_personality,
        "pidfd_getfd" => libc::SYS_pidfd_getfd,
        "pidfd_open" => libc::SYS_pidfd_open,
        "pidfd_send_signal" => libc::SYS_pidfd_send_signal,
        "pipe" => libc::SYS_pipe,
        "pipe2" => libc::SYS_pipe2,
        "pivot_root" => libc::SYS_pivot_root,
        "pkey_alloc" => libc::SYS_pkey_alloc,
        "pkey_free" => libc::SYS_pkey_free,
        "pkey_mprotect" => libc::SYS_pkey_mprotect,
        "poll" => libc::SYS_poll,
        "ppoll" => libc::SYS_ppoll,
        "prctl" => libc::SYS_prctl,
        "pread64" => libc::SYS_pread64,
        "preadv" => libc::SYS_preadv,
        "preadv2" => libc::SYS_preadv2,
        "prlimit64" => libc::SYS_prlimit64,
        "process_madvise" => libc::SYS_process_madvise,
        "process_mrelease" => libc::SYS_process_mrelease,
        "process_vm_readv" => libc::SYS_process_vm_readv,
        "process_vm_writev" => libc::SYS_process_vm_writev,
        "pselect6" => libc::SYS_pselect6,
        "ptrace" => libc::SYS_ptrace,
        "pwrite64" => libc::SYS_pwrite64,
        "pwritev" => libc::SYS_pwritev,
        "pwritev2" => libc::SYS_pwritev2,
        "read" => libc::SYS_read,
        "readahead" => libc::SYS_readahead,
        "readlink" => libc::SYS_readlink,
        "readlinkat" => libc::SYS_readlinkat,
        "readv" => libc::SYS_readv,
        "reboot" => libc::SYS_reboot,
        "recvfrom" => libc::SYS_recvfrom,
        "recvmmsg" => libc::SYS_recvmmsg,
        "recvmsg" => libc::SYS_recvmsg,
        "remap_file_pages" => libc::SYS_remap_file_pages,
        "removexattr" => libc::SYS_removexattr,
        "rename" => libc::SYS_rename,
        "renameat" => libc::SYS_renameat,
        "renameat2" => libc::SYS_renameat2,
        "restart_syscall" => libc::SYS_restart_syscall,
        "rmdir" => libc::SYS_rmdir,
        "rseq" => libc::SYS_rseq,
        "rt_sigaction" => libc::SYS_rt_sigaction,
        "rt_sigpending" => libc::SYS_rt_sigpending,
        "rt_sigprocmask" => libc::SYS_rt_sigprocmask,
        "rt_sigqueueinfo" => libc::SYS_rt_sigqueueinfo,
        "rt_sigreturn" => libc::SYS_rt_sigreturn,
        "rt_sigsuspend" => libc::SYS_rt_sigsuspend,
        "rt_sigtimedwait" => libc::SYS_rt_sigtimedwait,
        "rt_tgsigqueueinfo" => libc::SYS_rt_tgsigqueueinfo,
        "sched_get_priority_max" => libc::SYS_sched_get_priority_max,
        "sched_get_priority_min" => libc::SYS_sched_get_priority_min,
        "sched_getaffinity" => libc::SYS_sched_getaffinity,
        "sched_getattr" => libc::SYS_sched_getattr,
        "sched_getparam" => libc::SYS_sched_getparam,
        "sched_getscheduler" => libc::SYS_sched_getscheduler,
        "sched_rr_get_interval" => libc::SYS_sched_rr_get_interval,
        "sched_setaffinity" => libc::SYS_sched_setaffinity,
        "sched_setattr" => libc::SYS_sched_setattr,
        "sched_setparam" => libc::SYS_sched_setparam,
        "sched_setscheduler" => libc::SYS_sched_setscheduler,
        "sched_yield" => libc::SYS_sched_yield,
        "seccomp" => libc::SYS_seccomp,
        "select" => libc::SYS_select,
        "semctl" => libc::SYS_semctl,
        "semget" => libc::SYS_semget,
        "semop" => libc::SYS_semop,
        "semtimedop" => libc::SYS_semtimedop,
        "sendfile" => libc::SYS_sendfile,
        "sendmmsg" => libc::SYS_sendmmsg,
        "sendmsg" => libc::SYS_sendmsg,
        "sendto" => libc::SYS_sendto,
        "set_robust_list" => libc::SYS_set_robust_list,
        "set_tid_address" => libc::SYS_set_tid_address,
        "setfsgid" => libc::SYS_setfsgid,
        "setfsuid" => libc::SYS_setfsuid,
        "setgid" => libc::SYS_setgid,
        "setgroups" => libc::SYS_setgroups,
        "setitimer" => libc::SYS_setitimer,
        "setns" => libc::SYS_setns,
        "setpgid" => libc::SYS_setpgid,
        "setpriority" => libc::SYS_setpriority,
        "setregid" => libc::SYS_setregid,
        "setresgid" => libc::SYS_setresgid,
        "setresuid" => libc::SYS_setresuid,
        "setreuid" => libc::SYS_setreuid,
        "setrlimit" => libc::SYS_setrlimit,
        "setsid" => libc::SYS_setsid,
        "setsockopt" => libc::SYS_setsockopt,
        "settimeofday" => libc::SYS_settimeofday,
        "setuid" => libc::SYS_setuid,
        "setxattr" => libc::SYS_setxattr,
        "shmat" => libc::SYS_shmat,
        "shmctl" => libc::SYS_shmctl,
        "shmdt" => libc::SYS_shmdt,
        "shmget" => libc::SYS_shmget,
        "shutdown" => libc::SYS_shutdown,
        "sigaltstack" => libc::SYS_sigaltstack,
        "signalfd" => libc::SYS_signalfd,
        "signalfd4" => libc::SYS_signalfd4,
        "socket" => libc::SYS_socket,
        "socketpair" => libc::SYS_socketpair,
        "splice" => libc::SYS_splice,
        "stat" => libc::SYS_stat,
        "statfs" => libc::SYS_statfs,
        "statx" => libc::SYS_statx,
        "symlink" => libc::SYS_symlink,
        "symlinkat" => libc::SYS_symlinkat,
        "sync" => libc::SYS_sync,
        "sync_file_range" => libc::SYS_sync_file_range,
        "syncfs" => libc::SYS_syncfs,
        "sysinfo" => libc::SYS_sysinfo,
        "syslog" => libc::SYS_syslog,
        "tee" => libc::SYS_tee,
        "tgkill" => libc::SYS_tgkill,
        "time" => libc::SYS_time,
        "timer_create" => libc::SYS_timer_create,
        "timer_delete" => libc::SYS_timer_delete,
        "timer_getoverrun" => libc::SYS_timer_getoverrun,
        "timer_gettime" => libc::SYS_timer_gettime,
        "timer_settime" => libc::SYS_timer_settime,
        "timerfd_create" => libc::SYS_timerfd_create,
        "timerfd_gettime" => libc::SYS_timerfd_gettime,
        "timerfd_settime" => libc::SYS_timerfd_settime,
        "times" => libc::SYS_times,
        "tkill" => libc::SYS_tkill,
        "truncate" => libc::SYS_truncate,
        "umask" => libc::SYS_umask,
        "uname" => libc::SYS_uname,
        "unlink" => libc::SYS_unlink,
        "unlinkat" => libc::SYS_unlinkat,
        "utime" => libc::SYS_utime,
        "utimensat" => libc::SYS_utimensat,
        "utimes" => libc::SYS_utimes,
        "vfork" => libc::SYS_vfork,
        "vmsplice" => libc::SYS_vmsplice,
        "wait4" => libc::SYS_wait4,
        "waitid" => libc::SYS_waitid,
        "write" => libc::SYS_write,
        "writev" => libc::SYS_writev,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(json: &str) -> std::io::Result<CompiledSeccomp> {
        let p: OciSeccomp = serde_json::from_str(json).unwrap();
        compile(&p)
    }

    /// Minimal cBPF interpreter for the exact subset our compiler emits
    /// (`LD|W|ABS`, `JMP|JEQ|K`, `RET|K`). Returns the `SECCOMP_RET_*` value the
    /// program yields for a given `(arch, nr)`. This is what pins the CONTROL
    /// FLOW — the #108 first cut compiled to a valid program with the right ret
    /// VALUES but the wrong fall-through, which an index/value assertion missed
    /// but this simulator catches.
    fn run_bpf(prog: &[libc::sock_filter], arch: u32, nr: u32) -> u32 {
        let mut pc = 0usize;
        let mut acc: u32 = 0;
        loop {
            let insn = prog[pc];
            if insn.code == BPF_LD | BPF_W | BPF_ABS {
                acc = match insn.k {
                    SECCOMP_DATA_NR_OFFSET => nr,
                    SECCOMP_DATA_ARCH_OFFSET => arch,
                    other => panic!("unexpected LD offset {other}"),
                };
                pc += 1;
            } else if insn.code == BPF_JMP | BPF_JEQ | BPF_K {
                let taken = if acc == insn.k { insn.jt } else { insn.jf } as usize;
                pc += 1 + taken;
            } else if insn.code == BPF_RET | BPF_K {
                return insn.k;
            } else {
                panic!("unexpected opcode {:#x}", insn.code);
            }
        }
    }

    const I386_ARCH: u32 = 0x4000_0003; // AUDIT_ARCH_I386 — a "foreign" arch here.

    #[test]
    fn deny_list_selectively_blocks_only_listed_syscalls() {
        // default ALLOW, mkdir/mkdirat → ERRNO(EPERM). The filter MUST block ONLY
        // mkdir/mkdirat and ALLOW everything else (the bug was: it blocked all).
        let c = profile(
            r#"{ "defaultAction": "SCMP_ACT_ALLOW",
                 "syscalls": [ { "names": ["mkdir","mkdirat"], "action": "SCMP_ACT_ERRNO", "errnoRet": 1 } ] }"#,
        )
        .expect("supported profile compiles");
        // ld arch, jeq arch, ld nr, 2 jeqs, default-ret, listed-ret = 7.
        assert_eq!(c.prog.len(), 7, "expected flat 7-insn program");
        let nr = |n| syscall_nr(n).unwrap() as u32;
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("mkdir")),
            SECCOMP_RET_ERRNO | 1,
            "mkdir must be ERRNO(EPERM)"
        );
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("mkdirat")),
            SECCOMP_RET_ERRNO | 1,
            "mkdirat must be ERRNO(EPERM)"
        );
        // THE REGRESSION GUARD: a non-listed syscall MUST fall through to default ALLOW.
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("write")),
            SECCOMP_RET_ALLOW,
            "non-listed syscall (write) MUST fall through to default ALLOW, not the listed action"
        );
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("execve")),
            SECCOMP_RET_ALLOW,
            "non-listed syscall (execve) MUST be ALLOW"
        );
        // foreign arch → the default action (documented behavior).
        assert_eq!(
            run_bpf(&c.prog, I386_ARCH, nr("mkdir")),
            SECCOMP_RET_ALLOW,
            "foreign arch resolves to the default action"
        );
    }

    #[test]
    fn allow_list_default_deny_allows_only_listed() {
        // default ERRNO (deny), allow only write — the inverse shape. Proves the
        // compiler is correct for allow-lists too (default-deny is the Docker shape).
        let c = profile(
            r#"{ "defaultAction": "SCMP_ACT_ERRNO",
                 "syscalls": [ { "names": ["write"], "action": "SCMP_ACT_ALLOW" } ] }"#,
        )
        .expect("allow-list profile compiles");
        let nr = |n| syscall_nr(n).unwrap() as u32;
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("write")),
            SECCOMP_RET_ALLOW,
            "listed write must be ALLOW"
        );
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("mkdir")),
            SECCOMP_RET_ERRNO | 1,
            "non-listed syscall must hit default ERRNO"
        );
    }

    #[test]
    fn default_only_profile_compiles() {
        let c = profile(r#"{ "defaultAction": "SCMP_ACT_ERRNO", "syscalls": [] }"#)
            .expect("default-only compiles");
        // ld arch, jeq arch, ld nr, 0 jeqs, listed-ret, default-ret = 5.
        assert_eq!(c.prog.len(), 5);
    }

    #[test]
    fn arg_conditioned_entry_is_unsupported() {
        let r = profile(
            r#"{ "defaultAction": "SCMP_ACT_ALLOW",
                 "syscalls": [ { "names": ["ioctl"], "action": "SCMP_ACT_ERRNO",
                                 "args": [ { "index": 1, "value": 1, "op": "SCMP_CMP_EQ" } ] } ] }"#,
        );
        assert!(r.is_err(), "arg-conditioned rules must fail closed");
    }

    #[test]
    fn unknown_syscall_name_is_unsupported() {
        let r = profile(
            r#"{ "defaultAction": "SCMP_ACT_ALLOW",
                 "syscalls": [ { "names": ["totally_not_a_syscall"], "action": "SCMP_ACT_ERRNO" } ] }"#,
        );
        assert!(r.is_err(), "unknown syscall name must fail closed");
    }

    #[test]
    fn mixed_actions_are_unsupported() {
        let r = profile(
            r#"{ "defaultAction": "SCMP_ACT_ALLOW",
                 "syscalls": [ { "names": ["mkdir"], "action": "SCMP_ACT_ERRNO" },
                               { "names": ["rmdir"], "action": "SCMP_ACT_KILL" } ] }"#,
        );
        assert!(r.is_err(), "mixed per-syscall actions must fail closed");
    }

    #[test]
    fn builtin_default_profile_compiles_and_is_default_deny() {
        // The vendored `seccomp_default.json` parses + compiles (every listed name
        // must resolve in `syscall_nr`, or this fails closed) ...
        let c = compile_default().expect("built-in default profile compiles");
        let nr = |n| syscall_nr(n).unwrap() as u32;
        // ... a representative ALLOWED syscall falls through to the ALLOW block ...
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("read")),
            SECCOMP_RET_ALLOW,
            "an allow-listed syscall (read) must be ALLOW"
        );
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("arch_prctl")),
            SECCOMP_RET_ALLOW,
            "arch_prctl (C-runtime startup) must be ALLOW or every workload traps"
        );
        // ... and a syscall NOT in the allow-list hits the default ERRNO(EPERM),
        // proving this is a real default-deny allow-list (not allow-all).
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("ptrace")),
            SECCOMP_RET_ERRNO | 1,
            "a non-allow-listed syscall (ptrace) must be ERRNO(EPERM)"
        );
        assert_eq!(
            run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr("reboot")),
            SECCOMP_RET_ERRNO | 1,
            "a non-allow-listed syscall (reboot) must be ERRNO(EPERM)"
        );
    }

    #[test]
    fn unsupported_default_action_is_rejected() {
        let r = profile(r#"{ "defaultAction": "SCMP_ACT_TRACE", "syscalls": [] }"#);
        assert!(r.is_err(), "unsupported defaultAction must fail closed");
    }
}
