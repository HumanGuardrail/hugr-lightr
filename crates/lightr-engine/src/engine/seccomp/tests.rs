//! WP-#108 seccomp compiler unit tests (cBPF `run_bpf` simulator + shape/
//! selectivity/overflow guards). Extracted for the <=400-LOC godfile invariant.

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
    // ld arch, jeq arch, ret default, ld nr, 2×(jeq,ret), final ret = 4+2*2+1 = 9.
    assert_eq!(
        c.prog.len(),
        9,
        "expected inline-RET 9-insn program (offset-safe)"
    );
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
    // OVERFLOW GUARD: the default profile has ~289 entries, so its cBPF program
    // is >256 instructions. With the old far-jump layout the early JEQs' u8 jt
    // overflowed → wrong mapping; these allow-listed syscalls span EARLY
    // (arch_prctl above), MID, and LATE positions — all must still be ALLOW,
    // proving the inline-RET layout has no positional truncation at any size.
    for name in [
        "openat",
        "futex",
        "getrandom",
        "writev",
        "exit_group",
        "wait4",
    ] {
        assert_eq!(
                run_bpf(&c.prog, AUDIT_ARCH_X86_64, nr(name)),
                SECCOMP_RET_ALLOW,
                "allow-listed syscall {name} must be ALLOW regardless of its position in a >256-entry profile"
            );
    }
    // Foreign arch → the default action (inline foreign-RET, no far jump).
    assert_eq!(
        run_bpf(&c.prog, I386_ARCH, nr("read")),
        SECCOMP_RET_ERRNO | 1,
        "foreign arch resolves to the default action (ERRNO) in the default profile"
    );
}

#[test]
fn unsupported_default_action_is_rejected() {
    let r = profile(r#"{ "defaultAction": "SCMP_ACT_TRACE", "syscalls": [] }"#);
    assert!(r.is_err(), "unsupported defaultAction must fail closed");
}
