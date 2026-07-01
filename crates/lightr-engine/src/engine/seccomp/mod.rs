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

// WP godfile-split: the syscall-name→number table (~320 LOC on its own) lives in
// `syscalls.rs`; the compiler core + apply stay here. `syscall_nr` is `pub(super)`.
mod syscalls;
use syscalls::syscall_nr;

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
    let profile: OciSeccomp = serde_json::from_str(&buf).map_err(|e| {
        Error::new(
            ErrorKind::InvalidData,
            format!("seccomp profile parse: {e}"),
        )
    })?;
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

    // ── Build the cBPF program (OFFSET-SAFE for ANY number of syscalls) ───────
    // EVERY conditional jump uses jt/jf of ONLY 0 or 1 — never a far jump — so the
    // u8 jt/jf fields cannot overflow regardless of how many syscalls the profile
    // lists. (The earlier "flat JEQ → far listed-RET" layout silently TRUNCATED the
    // u8 offset once the listed-RET was >255 instructions away, i.e. for ~>252
    // syscalls — producing a WRONG filter: early JEQs jumped to garbage. That latent
    // #108 bug was exposed by the ~289-entry built-in default profile (#117): an
    // early syscall like `arch_prctl` mapped wrong ⇒ the workload trapped. This
    // inline-RET layout removes far jumps entirely.)
    //
    //   [0] LD  arch
    //   [1] JEQ arch == X86_64 ? jt=1 (skip the foreign-RET) : jf=0 (fall into it)
    //   [2] RET default            ; foreign/x32 arch → the default action (inline)
    //   [3] LD  nr
    //   per listed syscall (a 2-insn pair):
    //     JEQ nr ? jt=0 (fall to its RET) : jf=1 (skip its RET, try the next)
    //     RET listed-action
    //   [last] RET default         ; no syscall matched → the default action
    //
    // The DEFAULT action is what a non-matching syscall reaches (the final RET); a
    // MATCH falls into its own inline RET. (WP-#108's first cut inverted match vs
    // default → deny-all → musl SIGSEGV; the `run_bpf` simulator tests below pin
    // BOTH that selectivity AND — via a >256-entry profile — this no-overflow.)
    let n = nrs.len();
    let mut prog: Vec<libc::sock_filter> = Vec::with_capacity(4 + 2 * n + 1);

    // [0] LD arch
    prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARCH_OFFSET));
    // [1] JEQ arch == X86_64: match → skip the foreign-RET; foreign → fall into it.
    prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0));
    // [2] foreign/x32 arch → the default action (inline RET, no far jump).
    prog.push(stmt(BPF_RET | BPF_K, default_ret));
    // [3] LD nr
    prog.push(stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFFSET));
    // one (JEQ nr, RET listed) pair per listed syscall — all jt/jf are 0 or 1.
    for nr in &nrs {
        // match → jt=0 (fall to the RET below); no match → jf=1 (skip it).
        prog.push(jump(BPF_JMP | BPF_JEQ | BPF_K, *nr as u32, 0, 1));
        prog.push(stmt(BPF_RET | BPF_K, listed_ret));
    }
    // no syscall matched → the default action.
    prog.push(stmt(BPF_RET | BPF_K, default_ret));

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

#[cfg(test)]
mod tests;
