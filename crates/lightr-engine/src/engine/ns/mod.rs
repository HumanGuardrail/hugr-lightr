//! NsEngine — Linux user+mount+pid namespace isolation via unshare + pivot_root.

use super::Engine;
// Used only by the non-Linux stub below; the Linux `ns_impl` submodules import
// these directly.
#[cfg(not(target_os = "linux"))]
use super::spec::ExecSpec;
#[cfg(not(target_os = "linux"))]
use lightr_core::{LightrError, Result};

// ── Capability model (WP-#94) — pure, OS-agnostic logic ─────────────────────────
//
// The cap name→number table is the Linux uapi (`include/uapi/linux/capability.h`).
// `CAP_LAST_CAP` is the highest cap on a modern kernel (5.8+: CHECKPOINT_RESTORE).
// These helpers compute the DESIRED capability set from `--cap-drop`/`--cap-add`
// and are kept here (NOT inside the `cfg(target_os = "linux")` module) so the
// parsing + set algebra is unit-testable on any host; the Linux enforcement
// (`prctl`/`capset`) consumes the result. The lightr `ns` baseline is the FULL
// userns capability set (NOT Docker's default-14 subset — noted honestly; a
// future refinement could adopt Docker's default set), so:
//   desired = {0..=CAP_LAST_CAP}  −  cap_drop  +  cap_add
// `ALL` (case-insensitive) means every capability; entries are case-insensitive
// with an optional `CAP_` prefix. An unknown name is a hard error (fail-closed).

// These pure helpers are consumed by the Linux enforcement path (`ns_impl`) and
// by the host-agnostic unit tests; on a non-Linux NON-test build nothing calls
// them, so gate them to avoid dead-code warnings there (macOS `cargo build`).

/// Highest capability number this code knows about (Linux 5.8+: CHECKPOINT_RESTORE).
#[cfg(any(target_os = "linux", test))]
pub(crate) const CAP_LAST_CAP: u32 = 40;

/// Capability name → number (Linux uapi). The index in this slice IS the number,
/// so the table is also the 0..=CAP_LAST_CAP enumeration.
#[cfg(any(target_os = "linux", test))]
const CAP_NAMES: [&str; (CAP_LAST_CAP + 1) as usize] = [
    "CHOWN",              // 0
    "DAC_OVERRIDE",       // 1
    "DAC_READ_SEARCH",    // 2
    "FOWNER",             // 3
    "FSETID",             // 4
    "KILL",               // 5
    "SETGID",             // 6
    "SETUID",             // 7
    "SETPCAP",            // 8
    "LINUX_IMMUTABLE",    // 9
    "NET_BIND_SERVICE",   // 10
    "NET_BROADCAST",      // 11
    "NET_ADMIN",          // 12
    "NET_RAW",            // 13
    "IPC_LOCK",           // 14
    "IPC_OWNER",          // 15
    "SYS_MODULE",         // 16
    "SYS_RAWIO",          // 17
    "SYS_CHROOT",         // 18
    "SYS_PTRACE",         // 19
    "SYS_PACCT",          // 20
    "SYS_ADMIN",          // 21
    "SYS_BOOT",           // 22
    "SYS_NICE",           // 23
    "SYS_RESOURCE",       // 24
    "SYS_TIME",           // 25
    "SYS_TTY_CONFIG",     // 26
    "MKNOD",              // 27
    "LEASE",              // 28
    "AUDIT_WRITE",        // 29
    "AUDIT_CONTROL",      // 30
    "SETFCAP",            // 31
    "MAC_OVERRIDE",       // 32
    "MAC_ADMIN",          // 33
    "SYSLOG",             // 34
    "WAKE_ALARM",         // 35
    "BLOCK_SUSPEND",      // 36
    "AUDIT_READ",         // 37
    "PERFMON",            // 38
    "BPF",                // 39
    "CHECKPOINT_RESTORE", // 40
];

/// Normalize a cap token: trim, uppercase, strip an optional `CAP_` prefix.
#[cfg(any(target_os = "linux", test))]
fn normalize_cap(name: &str) -> String {
    let up = name.trim().to_ascii_uppercase();
    up.strip_prefix("CAP_").unwrap_or(&up).to_string()
}

/// Resolve a cap NAME to its number, or `None` if unknown.
#[cfg(any(target_os = "linux", test))]
fn cap_number(name: &str) -> Option<u32> {
    let n = normalize_cap(name);
    CAP_NAMES.iter().position(|&c| c == n).map(|i| i as u32)
}

/// Compute the DESIRED capability set from `cap_drop` then `cap_add`.
///
/// Start from the full userns set (`0..=CAP_LAST_CAP`), REMOVE every `cap_drop`
/// entry, then ADD every `cap_add` entry. `ALL` (case-insensitive) means every
/// capability (so `--cap-drop ALL` clears the set; `--cap-add ALL` restores it).
/// Order is drop-then-add, matching Docker (`--cap-drop ALL --cap-add NET_BIND_SERVICE`
/// ⇒ exactly `{NET_BIND_SERVICE}`). An unknown cap NAME is a hard error
/// (fail-closed — a typo'd security flag must never be silently ignored).
#[cfg(any(target_os = "linux", test))]
fn desired_caps(cap_drop: &[String], cap_add: &[String]) -> std::result::Result<Vec<u32>, String> {
    let all: Vec<u32> = (0..=CAP_LAST_CAP).collect();
    let mut set: std::collections::BTreeSet<u32> = all.iter().copied().collect();
    for c in cap_drop {
        if c.trim().eq_ignore_ascii_case("ALL") {
            set.clear();
        } else {
            let n =
                cap_number(c).ok_or_else(|| format!("unknown capability in --cap-drop: {c}"))?;
            set.remove(&n);
        }
    }
    for c in cap_add {
        if c.trim().eq_ignore_ascii_case("ALL") {
            set.extend(all.iter().copied());
        } else {
            let n = cap_number(c).ok_or_else(|| format!("unknown capability in --cap-add: {c}"))?;
            set.insert(n);
        }
    }
    Ok(set.into_iter().collect())
}

// ── NsEngine (Linux only) ─────────────────────────────────────────────────────
//
// The container launch core, split into focused submodules under `ns_impl` to
// keep each file small while preserving the module path and every unsafe/cfg/
// fail-closed detail. The `mod ns_impl { }` wrapper is retained (module-path
// stability); its children live at `ns/ns_impl/*.rs`:
//   engine   — NsEngine + Engine::run (shim-process orchestration)
//   run      — run_in_namespaces (the unshare/map/fork core)
//   rootfs   — PID-1 rootfs file writers + setup_rootfs_and_pivot phase
//   mounts   — /proc, /dev, /dev/shm, tmpfs, binds, netns join, loopback, uid-map
//   apply    — ulimits/oom/caps/apparmor/seccomp apply + apply_and_exec tail
//   user     — --user resolve + privilege drop
//   signal   — exec-readiness pipe, reaper loop, wait→exit-code
//   subid_ns — WP-#114 subuid RANGE plumbing (plan + parent dance)

#[cfg(target_os = "linux")]
mod ns_impl {
    mod apply;
    mod engine;
    mod mounts;
    mod rootfs;
    mod run;
    mod signal;
    mod subid_ns;
    mod user;

    pub(super) use engine::NsEngine;

    // The compiled seccomp filter carried from the pre-pivot COMPILE to the
    // pre-execv INSTALL. seccomp is x86_64-linux-only (AUDIT_ARCH_X86_64 by design;
    // the `syscall_nr` table uses x86_64 `libc::SYS_*`), so on other linux arches
    // the compiler module is absent and this type is UNINHABITED: a filter is never
    // constructed (`setup_rootfs_and_pivot` fails closed if one is requested) and the
    // install is a no-op. The `Option<SeccompFilter>` plumbing compiles on every arch.
    #[cfg(target_arch = "x86_64")]
    type SeccompFilter = crate::engine::seccomp::CompiledSeccomp;
    #[cfg(not(target_arch = "x86_64"))]
    enum SeccompFilter {}
}

#[cfg(target_os = "linux")]
pub(super) fn ns_engine_box() -> Box<dyn Engine> {
    Box::new(ns_impl::NsEngine)
}

/// macOS stub type so engine_for can name the arm — probe says unavailable,
/// so this is never actually constructed in production.
#[cfg(not(target_os = "linux"))]
struct NsEngineStub;

#[cfg(not(target_os = "linux"))]
impl Engine for NsEngineStub {
    fn run(&self, _spec: &ExecSpec) -> Result<i32> {
        Err(LightrError::InvalidRef(
            "ns engine requires Linux".to_string(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn ns_engine_box() -> Box<dyn Engine> {
    Box::new(NsEngineStub)
}

// ── WP-#94: capability-model unit tests (pure logic, host-agnostic) ─────────────
// These exercise the cap name→number table + `--cap-drop`/`--cap-add` set algebra,
// which is the security-critical parsing path. They need NO Linux (the prctl/capset
// enforcement is validated by the linux-validation `security-flags` job).
#[cfg(test)]
mod cap_tests {
    use super::{cap_number, desired_caps, normalize_cap, CAP_LAST_CAP};

    #[test]
    fn normalize_strips_cap_prefix_and_uppercases() {
        assert_eq!(normalize_cap("chown"), "CHOWN");
        assert_eq!(normalize_cap("CAP_NET_ADMIN"), "NET_ADMIN");
        assert_eq!(
            normalize_cap("  cap_net_bind_service  "),
            "NET_BIND_SERVICE"
        );
    }

    #[test]
    fn cap_number_known_and_unknown() {
        assert_eq!(cap_number("CHOWN"), Some(0));
        assert_eq!(cap_number("cap_chown"), Some(0));
        assert_eq!(cap_number("NET_BIND_SERVICE"), Some(10));
        assert_eq!(cap_number("SYS_ADMIN"), Some(21));
        assert_eq!(cap_number("CHECKPOINT_RESTORE"), Some(CAP_LAST_CAP));
        assert_eq!(cap_number("BOGUS_CAP"), None);
    }

    #[test]
    fn empty_drop_and_add_keeps_full_set() {
        let d = desired_caps(&[], &[]).unwrap();
        let all: Vec<u32> = (0..=CAP_LAST_CAP).collect();
        assert_eq!(d, all, "no flags ⇒ the full userns set is preserved");
    }

    #[test]
    fn drop_all_then_add_one_yields_exactly_one() {
        let d = desired_caps(&["ALL".to_string()], &["NET_BIND_SERVICE".to_string()]).unwrap();
        assert_eq!(
            d,
            vec![10],
            "--cap-drop ALL --cap-add NET_BIND_SERVICE ⇒ {{10}}"
        );
    }

    #[test]
    fn drop_all_with_cap_prefix_and_lowercase_add() {
        // Case-insensitivity + CAP_ prefix on the add side.
        let d = desired_caps(&["all".to_string()], &["cap_chown".to_string()]).unwrap();
        assert_eq!(d, vec![0]);
    }

    #[test]
    fn drop_single_removes_only_that_cap() {
        let d = desired_caps(&["CHOWN".to_string()], &[]).unwrap();
        assert!(!d.contains(&0), "CHOWN (0) must be dropped");
        assert!(d.contains(&1), "DAC_OVERRIDE (1) must remain");
        assert_eq!(d.len() as u32, CAP_LAST_CAP, "exactly one cap removed");
    }

    #[test]
    fn add_all_restores_after_drop_all() {
        let d = desired_caps(&["ALL".to_string()], &["ALL".to_string()]).unwrap();
        let all: Vec<u32> = (0..=CAP_LAST_CAP).collect();
        assert_eq!(d, all, "--cap-drop ALL --cap-add ALL ⇒ full set");
    }

    #[test]
    fn unknown_cap_is_hard_error_fail_closed() {
        // A typo'd security flag must FAIL, never be silently ignored.
        assert!(desired_caps(&["BOGUS_CAP".to_string()], &[]).is_err());
        assert!(desired_caps(&[], &["NOT_A_CAP".to_string()]).is_err());
    }
}
