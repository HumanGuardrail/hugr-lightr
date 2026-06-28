//! NsEngine — Linux user+mount+pid namespace isolation via unshare + pivot_root.

use super::spec::ExecSpec;
use super::Engine;
// Used only by the non-Linux stub below; the Linux `ns_impl` has its own import.
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
    "CHOWN",            // 0
    "DAC_OVERRIDE",     // 1
    "DAC_READ_SEARCH",  // 2
    "FOWNER",           // 3
    "FSETID",           // 4
    "KILL",             // 5
    "SETGID",           // 6
    "SETUID",           // 7
    "SETPCAP",          // 8
    "LINUX_IMMUTABLE",  // 9
    "NET_BIND_SERVICE", // 10
    "NET_BROADCAST",    // 11
    "NET_ADMIN",        // 12
    "NET_RAW",          // 13
    "IPC_LOCK",         // 14
    "IPC_OWNER",        // 15
    "SYS_MODULE",       // 16
    "SYS_RAWIO",        // 17
    "SYS_CHROOT",       // 18
    "SYS_PTRACE",       // 19
    "SYS_PACCT",        // 20
    "SYS_ADMIN",        // 21
    "SYS_BOOT",         // 22
    "SYS_NICE",         // 23
    "SYS_RESOURCE",     // 24
    "SYS_TIME",         // 25
    "SYS_TTY_CONFIG",   // 26
    "MKNOD",            // 27
    "LEASE",            // 28
    "AUDIT_WRITE",      // 29
    "AUDIT_CONTROL",    // 30
    "SETFCAP",          // 31
    "MAC_OVERRIDE",     // 32
    "MAC_ADMIN",        // 33
    "SYSLOG",           // 34
    "WAKE_ALARM",       // 35
    "BLOCK_SUSPEND",    // 36
    "AUDIT_READ",       // 37
    "PERFMON",          // 38
    "BPF",              // 39
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
fn desired_caps(
    cap_drop: &[String],
    cap_add: &[String],
) -> std::result::Result<Vec<u32>, String> {
    let all: Vec<u32> = (0..=CAP_LAST_CAP).collect();
    let mut set: std::collections::BTreeSet<u32> = all.iter().copied().collect();
    for c in cap_drop {
        if c.trim().eq_ignore_ascii_case("ALL") {
            set.clear();
        } else {
            let n = cap_number(c).ok_or_else(|| format!("unknown capability in --cap-drop: {c}"))?;
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

#[cfg(target_os = "linux")]
mod ns_impl {
    use super::super::seccomp;
    use super::super::spec::{BindMount, TmpfsMount, Ulimit};
    use super::{desired_caps, Engine, ExecSpec, CAP_LAST_CAP};
    use lightr_core::{LightrError, Result};
    use std::ffi::CString;

    pub struct NsEngine;

    impl Engine for NsEngine {
        fn run(&self, spec: &ExecSpec) -> Result<i32> {
            let rootfs = spec.rootfs.ok_or_else(|| {
                LightrError::InvalidRef("ns engine requires a rootfs".to_string())
            })?;

            let rootfs_path = rootfs.to_owned();
            let cwd_str = spec.cwd.to_string_lossy().into_owned();
            let command: Vec<String> = spec.command.to_vec();
            // CRITEST "starting container": critest starts containers with a BARE
            // command (`top`). The ns engine `execv`s with the INHERITED env (it does
            // not `execve`), so the workload's PATH is the value carried in `spec.env`
            // — capture it (cloned, like `command`) pre-fork so PID 1 owns its copy and
            // can execvp-resolve argv[0] against the CONTAINER's PATH post-pivot. `None`
            // ⇒ the standard default PATH is used (see `pathres::resolve_in_path`).
            let env_path: Option<String> = crate::pathres::path_from_env(spec.env);
            let limits = spec.limits;
            // WP-NET-ISO: `--net=none` ⇒ create a network namespace (CLONE_NEWNET)
            // so the container gets an isolated, empty net stack (loopback only).
            let net_isolate = spec.net_isolate;
            // WP-#92: `--read-only` ⇒ remount the pivoted rootfs RO; `--shm-size`
            // ⇒ a sized `/dev/shm` tmpfs (None ⇒ a default 64 MiB mount).
            let read_only = spec.read_only;
            let shm_size = spec.shm_size;
            // WP-#94: `--cap-drop`/`--cap-add` — Linux capabilities to drop/add.
            // Captured (cloned) before fork, like `command`, so the child owns them.
            let cap_drop: Vec<String> = spec.cap_drop.to_vec();
            let cap_add: Vec<String> = spec.cap_add.to_vec();
            // WP-#95: `--init` ⇒ run a minimal PID-1 reaper inside the new pid ns
            // (the workload becomes PID 2); false ⇒ the workload is PID 1 directly.
            let init = spec.init;
            // WP-#99: JOIN an existing (CNI-pinned) netns instead of creating one,
            // and an EXPLICIT cgroup leaf name. Captured (owned) before the fork so
            // the child owns its copy, exactly like `command`/`cap_*`.
            let join_netns: Option<std::path::PathBuf> = spec.join_netns.map(|p| p.to_owned());
            let cgroup_name: Option<String> = spec.cgroup_name.map(|s| s.to_owned());
            // WP-#102: the write end of the exec-readiness pipe (a raw fd number),
            // captured pre-fork like `cgroup_name`. `None` ⇒ byte-identical to the
            // pre-#102 path (no pipe, no close-points, no CLOEXEC dance). Each level
            // CLOSES its copy after forking the next, so only PID 1 holds it.
            let exec_ready_fd: Option<libc::c_int> = spec.exec_ready_fd;
            // WP-#106: the AppArmor profile NAME to exec the workload under (a loaded
            // profile for CRI `Localhost`, or "unconfined" to explicitly run
            // unconfined; `None` ⇒ no change / inherit). Captured (owned) pre-fork like
            // `command`/`cap_*` so PID 1 owns its copy; applied as the LAST pre-execv
            // step (after caps), fail-closed. `None` ⇒ byte-identical to the pre-#106
            // path (no attr write).
            let apparmor: Option<String> = spec.apparmor.map(|s| s.to_owned());
            // WP-#108 (seccomp): the PATH to an OCI seccomp JSON profile (or
            // "unconfined") to enforce on the workload. Captured (owned) pre-fork like
            // `apparmor` so PID 1 owns its copy. PID 1 COMPILES it early (before
            // pivot_root, while the host path is visible) and INSTALLS it late (after
            // the apparmor apply, right before execv), fail-closed. `None`/"unconfined"
            // ⇒ no filter (byte-identical to the pre-#108 path for `None`).
            let seccomp: Option<String> = spec.seccomp.map(|s| s.to_owned());
            // `--user` (uid/gid switch, Docker parity): the `uid[:gid]` or
            // `name[:group]` spec to drop the workload to. Captured (owned) pre-fork
            // like `apparmor`/`seccomp` so PID 1 owns its copy. Resolved + applied LATE
            // (after the apparmor apply, BEFORE seccomp install — so the filter never
            // blocks setuid, and the switch happens while we still hold CAP_SETUID/SETGID
            // from the userns baseline), fail-closed against the CONTAINER /etc/passwd.
            // `None` ⇒ no switch (byte-identical to the pre-feature path).
            let user: Option<String> = spec.user.map(|s| s.to_owned());
            // WP-#107 (CRI GAP 1/2/3): CRI container/sandbox setup the ns engine must
            // honor — volume bind mounts, the synthesized /etc/resolv.conf content, and
            // the sandbox hostname (which also drives a CLONE_NEWUTS unshare). Captured
            // (cloned/owned) pre-fork like `command`/`cap_*` so PID 1 owns its copies.
            // All three default to empty/None ⇒ byte-identical to the pre-#107 path.
            let bind_mounts: Vec<BindMount> = spec.bind_mounts.to_vec();
            let resolv_conf: Option<String> = spec.resolv_conf.map(|s| s.to_owned());
            let hostname: Option<String> = spec.hostname.map(|s| s.to_owned());
            // `--add-host` (Docker parity): (hostname, ip) pairs appended to the
            // container's /etc/hosts before pivot. `--tmpfs` (Docker parity): tmpfs
            // targets mounted after the /dev/shm setup. Both captured (cloned) pre-fork
            // like the CRI fields above so PID 1 owns its copies. Empty ⇒ unchanged.
            let add_host: Vec<(String, String)> = spec.add_host.to_vec();
            let tmpfs: Vec<TmpfsMount> = spec.tmpfs.to_vec();
            // `--ulimit` (Docker parity): per-process `setrlimit` caps. Captured
            // (cloned) pre-fork like `tmpfs`/`cap_*` so PID 1 owns its copy; applied
            // EARLY in PID 1 (before the caps/user/seccomp block — a hard-limit raise
            // still holds CAP_SYS_RESOURCE there, a lowering always works). Empty ⇒
            // no-op (byte-identical to the pre-feature path).
            let ulimits: Vec<Ulimit> = spec.ulimits.to_vec();

            // WP-#95: fork the SETUP process. NOTE (corrected from a wrong pre-#95
            // comment): this child does NOT become PID 1 — `unshare(CLONE_NEWPID)`
            // only places the unsharer's FIRST CHILD into the new pid namespace, and
            // this child is the unsharer, not its child. It is the *external* parent
            // of PID 1; it performs ONLY the unshare-process setup (userns map, cgroup,
            // optional loopback, caps PARSE) while holding CAP_SYS_ADMIN in the new
            // userns, then forks AGAIN inside `run_in_namespaces` so the grandchild is
            // PID 1 — and PID 1 does ALL rootfs setup (mounts, fresh /proc, pivot_root)
            // so the procfs is mounted while the host /proc is still fully-visible (#96).
            // Safety: standard fork pattern; the child runs setup then forks+execs.
            let pid = unsafe { libc::fork() };
            match pid {
                -1 => Err(LightrError::Io(std::io::Error::last_os_error())),
                0 => {
                    // ── child ──────────────────────────────────────────────
                    let rc = run_in_namespaces(
                        &rootfs_path,
                        &cwd_str,
                        &command,
                        env_path.as_deref(),
                        &limits,
                        net_isolate,
                        read_only,
                        shm_size,
                        &cap_drop,
                        &cap_add,
                        init,
                        join_netns.as_deref(),
                        cgroup_name.as_deref(),
                        exec_ready_fd,
                        apparmor.as_deref(),
                        seccomp.as_deref(),
                        user.as_deref(),
                        &bind_mounts,
                        resolv_conf.as_deref(),
                        hostname.as_deref(),
                        &add_host,
                        &tmpfs,
                        &ulimits,
                    );
                    std::process::exit(rc);
                }
                child_pid => {
                    // ── parent: wait ───────────────────────────────────────
                    // WP-#102: this is the `run()` parent — the `__ns-run` SHIM
                    // process. It inherited the pipe write end across the spawn but
                    // must NOT keep it: only PID 1 may hold it, or EOF (the
                    // exec-success signal) would never fire while the shim lives.
                    // Close BEFORE waitpid (which blocks for the container's life).
                    if let Some(fd) = exec_ready_fd {
                        unsafe { libc::close(fd) };
                    }
                    let mut wstatus: libc::c_int = 0;
                    let r = unsafe { libc::waitpid(child_pid, &mut wstatus, 0) };
                    if r == -1 {
                        return Err(LightrError::Io(std::io::Error::last_os_error()));
                    }
                    Ok(wait_to_exit_code(wstatus))
                }
            }
        }
    }

    fn wait_to_exit_code(wstatus: libc::c_int) -> i32 {
        if libc::WIFEXITED(wstatus) {
            libc::WEXITSTATUS(wstatus)
        } else if libc::WIFSIGNALED(wstatus) {
            128 + libc::WTERMSIG(wstatus)
        } else {
            1
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_in_namespaces(
        rootfs: &std::path::Path,
        cwd: &str,
        command: &[String],
        // CRITEST "starting container": the workload's PATH (from `spec.env`), used to
        // execvp-resolve a bare argv[0] against the CONTAINER rootfs post-pivot. `None`
        // ⇒ the standard default PATH (`pathres::DEFAULT_PATH`).
        env_path: Option<&str>,
        limits: &lightr_core::ResourceLimits,
        net_isolate: bool,
        read_only: bool,
        shm_size: Option<u64>,
        cap_drop: &[String],
        cap_add: &[String],
        init: bool,
        join_netns: Option<&std::path::Path>,
        cgroup_name: Option<&str>,
        exec_ready_fd: Option<libc::c_int>,
        apparmor: Option<&str>,
        // WP-#108 (seccomp): the PATH to an OCI seccomp JSON profile (or
        // "unconfined") to enforce on the workload. Compiled EARLY (pre-pivot, host
        // path visible) + installed LATE (pre-execv), fail-closed. `None`/"unconfined"
        // ⇒ no filter.
        seccomp: Option<&str>,
        // `--user` (uid/gid switch): the `uid[:gid]` / `name[:group]` spec to drop the
        // workload to. Resolved against the CONTAINER /etc/passwd|/etc/group (post-pivot)
        // and applied (setgroups→setgid→setuid) AFTER apparmor and BEFORE seccomp, while
        // CAP_SETUID/SETGID are still held. `None` ⇒ no switch.
        user: Option<&str>,
        // WP-#107 (CRI GAP 1/2/3): CRI volume bind mounts, the synthesized
        // /etc/resolv.conf content, and the sandbox hostname. Empty/None ⇒ the
        // pre-#107 path is unchanged.
        bind_mounts: &[BindMount],
        resolv_conf: Option<&str>,
        hostname: Option<&str>,
        // `--add-host`: (hostname, ip) pairs appended to <rootfs>/etc/hosts BEFORE
        // pivot (alongside resolv.conf/hostname). `--tmpfs`: tmpfs targets mounted
        // AFTER /dev/shm. Empty ⇒ the pre-feature path is unchanged.
        add_host: &[(String, String)],
        tmpfs: &[TmpfsMount],
        // `--ulimit`: per-process `setrlimit` caps applied in PID 1, EARLY (before
        // the caps/user/seccomp block). Empty ⇒ the pre-feature path is unchanged.
        ulimits: &[Ulimit],
    ) -> i32 {
        // WP-#99: JOIN the pod's existing netns BEFORE `unshare(CLONE_NEWUSER)`.
        // THE ordering rule: we must `setns(CLONE_NEWNET)` while still real root in
        // the HOST init userns — the pinned netns is owned by the host userns, and a
        // CHILD userns (post-unshare) holds NO capabilities over it, so a join after
        // the userns unshare EPERMs. So this is the very first thing we do. Joining a
        // netns and creating one (`net_isolate`) are mutually exclusive — join wins.
        // Fail-closed: any failure returns 1 (the setup process, pre-fork, so a
        // `return` is correct here — we are NOT yet PID 1).
        if let Some(ns_path) = join_netns {
            if let Err(e) = setns_netns(ns_path) {
                eprintln!("lightr-engine ns: join netns {ns_path:?} failed: {e}");
                return 1;
            }
        }

        // Capture the REAL outer uid/gid BEFORE unsharing the user namespace.
        // After unshare(CLONE_NEWUSER) and before a map is written, the process
        // has no mapping, so getuid()/getgid() return the overflow id (65534);
        // writing "0 65534 1" to uid_map is rejected by the kernel (the map must
        // target the writer's real id in the parent userns). Reading first fixes
        // the "uid/gid map failed" that broke the ns engine on Linux.
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        // unshare user+mount+pid namespaces. WP-NET-ISO: with `--net=none`, also
        // unshare the network namespace (CLONE_NEWNET) so the container starts
        // with an isolated, empty net stack (loopback only); host interfaces and
        // ports are invisible. When net_isolate=false the flags are byte-identical
        // to before (share host network — zero regression).
        let mut flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID;
        // WP-#99: when JOINING a netns we already `setns`'d into it above and must
        // NOT also create a fresh one — `join_netns` wins over `net_isolate`.
        if net_isolate && join_netns.is_none() {
            flags |= libc::CLONE_NEWNET;
        }
        // WP-#107 (CRI GAP 3, "set hostname"): when the sandbox sets a hostname we
        // must own a private UTS namespace so `sethostname` (in PID 1) changes ONLY
        // the container's hostname, not the host's. Added to the SAME unshare set as
        // USER/NS/PID — it does not disturb the netns-join/userns ordering (#99–#106:
        // the netns join already happened via setns ABOVE; this is the fresh-unshare
        // set). `None` ⇒ no UTS ns (unchanged behavior).
        if hostname.is_some() {
            flags |= libc::CLONE_NEWUTS;
        }
        if unsafe { libc::unshare(flags) } != 0 {
            eprintln!(
                "lightr-engine ns: unshare failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }

        // Map uid 0 inside → real outer uid (captured above)
        if write_map("/proc/self/uid_map", &format!("0 {} 1\n", uid)).is_err()
            || write_map("/proc/self/setgroups", "deny\n").is_err()
            || write_map("/proc/self/gid_map", &format!("0 {} 1\n", gid)).is_err()
        {
            eprintln!("lightr-engine ns: uid/gid map failed");
            return 1;
        }

        // F-203 (#90): apply cgroup v2 caps (memory.max / cpu.max / pids.max) HERE
        // — after the uid/gid map, but **before** the mount sequence + pivot_root.
        // The mount namespace is unshared yet still carries the host mounts, so the
        // host's cgroup-v2 hierarchy is still visible at `/sys/fs/cgroup`. After
        // pivot_root the root is the container rootfs whose `/sys/fs/cgroup` is an
        // empty dir, so apply_cgroup there always failed "cgroup v2 not mounted"
        // (the Linux-CI resource-limits job exposed this — the caps never actually
        // applied). cgroup membership survives the later mount-ns pivot, and exec
        // inherits it. Fail closed: any error returns 1.
        // WP-#99: `cgroup_name`, when set, pins the leaf so the CRI backend's
        // `stop` can `cgroup.kill` the whole subtree; it also forces leaf creation
        // even when limits are unlimited (so the container is always killable).
        if let Err(e) = crate::limits::apply_cgroup(limits, cgroup_name) {
            eprintln!("lightr-engine ns: apply_cgroup failed: {e}");
            return 1;
        }

        // WP-NET-ISO: with `--net=none` the new netns starts with `lo` DOWN, so
        // even loopback traffic fails. Bring `lo` up here — we hold CAP_NET_ADMIN
        // in the new userns, so this works rootless. MUST run AFTER the uid/gid
        // map is written (the cap is only effective once the map exists) and
        // BEFORE pivot_root/exec. Fail closed: any error returns 1. WP-#99: SKIP
        // when joining an existing netns (the pod's `lo` is already configured by
        // CNI; we hold no NET_ADMIN over the host-owned netns from our child userns
        // anyway — `join_netns` wins over the `net_isolate` loopback path).
        if net_isolate && join_netns.is_none() {
            if let Err(e) = bring_up_loopback() {
                eprintln!("lightr-engine ns: bring up loopback failed: {e}");
                return 1;
            }
        }

        // WP-#96: the rootfs setup (MS_PRIVATE → bind → fresh procfs → pivot_root →
        // /dev → /dev/shm → optional read-only remount → chdir cwd) is NO LONGER done
        // here. It has been RELOCATED into PID 1 (the grandchild forked below) so the
        // fresh procfs can be mounted while the host `/proc` (inherited via
        // CLONE_NEWNS) is still fully-visible — the only point at which a rootless
        // fresh procfs mount is legal (`mount_too_revealing`/`fs_fully_visible`). See
        // the PID-1 branch for the moved block.

        // WP-#94/#95/#96: COMPUTE the desired capability set now (pure parse; fail-closed
        // on an unknown name), but DEFER applying it until the process that actually
        // execs the workload — capping THIS setup process would not cap the workload
        // (it execs in a forked descendant; see the pid-ns fork below). When neither
        // `--cap-*` flag is set we keep `None` ⇒ the full userns set is preserved
        // (ordinary runs are byte-identical to before this WP). This pure parse runs in
        // the SETUP process before the PID-1 fork; the resulting set is copied into PID
        // 1 across the fork and applied there, right before exec.
        let desired_caps_vec: Option<Vec<u32>> = if !cap_drop.is_empty() || !cap_add.is_empty() {
            match desired_caps(cap_drop, cap_add) {
                Ok(d) => Some(d),
                Err(e) => {
                    eprintln!("lightr-engine ns: {e}");
                    return 1;
                }
            }
        } else {
            None
        };

        // Validate + prepare the exec argv BEFORE the pid-ns fork (so both the parent
        // error path and the forked children share the prepared CStrings; `fork`
        // copies the address space, so the child owns its copy).
        if command.is_empty() {
            eprintln!("lightr-engine ns: empty command");
            return 1;
        }
        // Early NUL-validate argv[0] in the SETUP process (a return-able error, before
        // the pid-ns fork) — an interior NUL is a malformed command. The program string
        // is RE-resolved against the container PATH post-pivot in PID 1
        // (`pathres::resolve_in_path`), which builds the actual exec CString; this check
        // just rejects a bad name early with a clean `return 1` rather than a post-fork
        // `_exit`. (`_`-prefixed: validation only, not the exec'd pointer.)
        let _prog_c = match CString::new(command[0].as_bytes()) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("lightr-engine ns: bad program name");
                return 1;
            }
        };
        let argv_c: Vec<CString> = command
            .iter()
            .filter_map(|s| CString::new(s.as_bytes()).ok())
            .collect();
        let mut argv_ptrs: Vec<*const libc::c_char> = argv_c.iter().map(|c| c.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());

        // WP-#95: the REAL pid-namespace fix. `unshare(CLONE_NEWPID)` above does NOT
        // move this (setup) process into the new pid namespace — per `man 2 unshare`,
        // only the unsharer's FIRST CHILD becomes PID 1 there. So we MUST fork here:
        // the grandchild is PID 1 in the new ns and execs (or, with `--init`, forks)
        // the workload. Pre-#95 the code exec'd WITHOUT this fork, leaving the
        // workload in the HOST pid namespace (false isolation — the confirmed bug).
        // This setup process stays the EXTERNAL parent of PID 1: it waitpids it and
        // propagates the exit code up to `run()`'s waitpid (3 levels total).
        let workload_pid = unsafe { libc::fork() };
        if workload_pid < 0 {
            eprintln!(
                "lightr-engine ns: pid-ns fork failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }
        if workload_pid == 0 {
            // ── grandchild: PID 1 in the NEW pid namespace ───────────────────────
            // WP-#96: PID 1 does ALL rootfs setup (relocated here from the setup
            // process). The crux: the fresh procfs is mounted (step 3) BEFORE
            // pivot_root + the put_old MNT_DETACH, while the host `/proc` inherited via
            // CLONE_NEWNS is still fully-visible — so the kernel
            // (`mount_too_revealing`/`fs_fully_visible`) permits a rootless fresh
            // procfs mount; and PID 1 is in the new pid ns, so that proc reflects the
            // new ns (`cat /proc/self/status` ⇒ `Pid: 1`). Post-pivot there would be
            // no fully-visible proc left → EPERM (the bug #96 fixes).
            // We are POST-fork here, so every failure MUST `libc::_exit(1)` (NOT
            // `return 1`, which was correct only in the setup process).

            // Build the rootfs CString (rebuilt here; the setup-process copy is gone).
            let rootfs_c = match CString::new(rootfs.as_os_str().as_encoded_bytes()) {
                Ok(c) => c,
                Err(_) => {
                    eprintln!("lightr-engine ns: bad rootfs path");
                    signal_setup_failed(exec_ready_fd, "bad rootfs path"); // WP-#104
                    unsafe { libc::_exit(1) }
                }
            };
            let none = CString::new("none").unwrap();
            let empty = CString::new("").unwrap();

            // 1. Make root mount private
            let r = unsafe {
                libc::mount(
                    none.as_ptr(),
                    c"/".as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                )
            };
            if r != 0 {
                eprintln!(
                    "lightr-engine ns: MS_PRIVATE on / failed: {}",
                    std::io::Error::last_os_error()
                );
                signal_setup_failed(exec_ready_fd, "MS_PRIVATE on / failed"); // WP-#104
                unsafe { libc::_exit(1) };
            }

            // 2. Bind-mount rootfs onto itself so it becomes a mountpoint for pivot_root
            let r = unsafe {
                libc::mount(
                    rootfs_c.as_ptr(),
                    rootfs_c.as_ptr(),
                    empty.as_ptr(),
                    libc::MS_BIND | libc::MS_REC,
                    std::ptr::null(),
                )
            };
            if r != 0 {
                eprintln!(
                    "lightr-engine ns: bind-mount rootfs failed: {}",
                    std::io::Error::last_os_error()
                );
                signal_setup_failed(exec_ready_fd, "bind-mount rootfs failed"); // WP-#104
                unsafe { libc::_exit(1) };
            }

            // 3. WP-#96: mount a FRESH procfs at <rootfs>/proc — BEFORE pivot_root,
            // while the host /proc is still fully-visible (the crux of the fix) and PID
            // 1 is in the new pid ns. Best-effort (log + continue), but it should now
            // succeed; the CI hard-requires it.
            mount_proc(&rootfs.join("proc"));

            // 3b. WP-#107 (CRI GAP 2, "DNS config"): write the synthesized
            // /etc/resolv.conf into the rootfs BEFORE pivot_root (Docker/runc do this),
            // overwriting whatever the image carried. We write through the
            // still-unpivoted rootfs path (`<rootfs>/etc/resolv.conf`). `None` ⇒ skip
            // entirely (image resolv.conf untouched). Fail-closed: a requested DNS
            // config that cannot be written is a real error (the kubelet asked for
            // specific resolvers — silently dropping them is a conformance lie).
            if let Some(content) = resolv_conf {
                if let Err(e) = write_rootfs_file(rootfs, "etc/resolv.conf", content.as_bytes()) {
                    eprintln!("lightr-engine ns: write /etc/resolv.conf failed: {e}");
                    signal_setup_failed(exec_ready_fd, "write /etc/resolv.conf failed"); // WP-#107
                    unsafe { libc::_exit(1) };
                }
            }

            // 3c. WP-#107 (CRI GAP 3, "set hostname"): write /etc/hostname into the
            // rootfs BEFORE pivot_root (runc writes BOTH the file and calls
            // sethostname). The kernel-level `sethostname` is done just below (we are
            // in the new UTS ns from the unshare). `None` ⇒ skip. Fail-closed: a
            // requested hostname that cannot be written is a real error.
            if let Some(name) = hostname {
                let mut line = name.as_bytes().to_vec();
                line.push(b'\n');
                if let Err(e) = write_rootfs_file(rootfs, "etc/hostname", &line) {
                    eprintln!("lightr-engine ns: write /etc/hostname failed: {e}");
                    signal_setup_failed(exec_ready_fd, "write /etc/hostname failed"); // WP-#107
                    unsafe { libc::_exit(1) };
                }
                // sethostname in the new UTS ns (we hold CAP_SYS_ADMIN in the userns).
                // Fail-closed: a requested hostname that cannot be set is a real error.
                let bytes = name.as_bytes();
                let r = unsafe {
                    libc::sethostname(bytes.as_ptr() as *const libc::c_char, bytes.len())
                };
                if r != 0 {
                    let e = std::io::Error::last_os_error();
                    eprintln!("lightr-engine ns: sethostname({name:?}) failed: {e}");
                    signal_setup_failed(exec_ready_fd, "sethostname failed"); // WP-#107
                    unsafe { libc::_exit(1) };
                }
            }

            // 3c2. `--add-host` (Docker parity): APPEND each `(hostname, ip)` entry to
            // <rootfs>/etc/hosts as a `"<ip>\t<hostname>"` line, BEFORE pivot_root
            // (Docker/runc write the container's /etc/hosts host-side). APPEND — we
            // preserve any /etc/hosts the image carried (e.g. the default
            // `127.0.0.1 localhost`); the file (and /etc) is created if missing.
            // Mirrors the resolv.conf/hostname writes above but uses an APPEND helper.
            // Empty ⇒ skip (image /etc/hosts untouched). Fail-closed: a requested
            // host mapping that cannot be written is a real error (silently dropping
            // it would be a parity lie).
            if !add_host.is_empty() {
                let mut block = Vec::new();
                for (host, ip) in add_host {
                    block.extend_from_slice(ip.as_bytes());
                    block.push(b'\t');
                    block.extend_from_slice(host.as_bytes());
                    block.push(b'\n');
                }
                if let Err(e) = append_rootfs_file(rootfs, "etc/hosts", &block) {
                    eprintln!("lightr-engine ns: write /etc/hosts failed: {e}");
                    signal_setup_failed(exec_ready_fd, "write /etc/hosts failed");
                    unsafe { libc::_exit(1) };
                }
            }

            // 3d. WP-#107 (CRI GAP 1, "starting container with volume"): apply the CRI
            // volume bind mounts. MUST be done BEFORE pivot_root: the `host_path` is a
            // HOST path, only reachable while the host fs is still mounted — after
            // pivot_root + the put_old MNT_DETACH it is gone (the same reason the /dev
            // binds reference the old root pre-unmount). We bind into the rootfs at
            // `<rootfs>/<container_path>` (still the pre-pivot path), `mkdir -p`ing the
            // target first (the image may lack it). `host_path` is already realpath'd
            // host-side in build_ns_plan (the symlink-host-path spec). Fail-closed: a
            // volume that cannot be applied aborts the start (a missing volume is a real
            // error). Empty ⇒ skipped (unchanged behavior).
            for m in bind_mounts {
                if let Err(e) = apply_bind_mount(rootfs, m) {
                    eprintln!(
                        "lightr-engine ns: bind mount {:?} -> {:?} failed: {e}",
                        m.host_path, m.container_path
                    );
                    signal_setup_failed(exec_ready_fd, "volume bind mount failed"); // WP-#107
                    unsafe { libc::_exit(1) };
                }
            }

            // WP-#108 (seccomp), EARLY half: COMPILE the OCI seccomp profile NOW —
            // BEFORE pivot_root, while the HOST profile path is still reachable (after
            // the put_old MNT_DETACH it is gone, exactly like the CRI volume host_paths
            // above). The compiled cBPF program (a plain Vec) is held in a local and
            // INSTALLED late (after the apparmor apply, right before execv), so the
            // filter is armed last — never restricting PID 1's own rootfs setup. A
            // `Some("unconfined")` profile compiles to None (explicit no-op). Fail-
            // closed: an unreadable/unparseable/unsupported profile `_exit`s rather
            // than exec unfiltered (the same discipline as #106 AppArmor). `None` ⇒
            // byte-identical to the pre-#108 path.
            let compiled_seccomp: Option<seccomp::CompiledSeccomp> = match seccomp {
                Some(p) if p != "unconfined" => match seccomp::compile_from_path(p) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        eprintln!("lightr-engine ns: seccomp: {e}");
                        signal_setup_failed(exec_ready_fd, &format!("seccomp: {e}"));
                        unsafe { libc::_exit(1) };
                    }
                },
                _ => None,
            };

            // 4. Create put_old dir inside rootfs, then pivot_root
            let put_old = rootfs.join(".put_old");
            if std::fs::create_dir_all(&put_old).is_err() {
                eprintln!("lightr-engine ns: cannot create .put_old");
                signal_setup_failed(exec_ready_fd, "cannot create .put_old"); // WP-#104
                unsafe { libc::_exit(1) };
            }

            let put_old_c = match CString::new(put_old.as_os_str().as_encoded_bytes()) {
                Ok(c) => c,
                Err(_) => {
                    eprintln!("lightr-engine ns: bad put_old path");
                    signal_setup_failed(exec_ready_fd, "bad put_old path"); // WP-#104
                    unsafe { libc::_exit(1) }
                }
            };

            let r = unsafe {
                libc::syscall(libc::SYS_pivot_root, rootfs_c.as_ptr(), put_old_c.as_ptr())
            };
            if r != 0 {
                eprintln!(
                    "lightr-engine ns: pivot_root failed: {}",
                    std::io::Error::last_os_error()
                );
                signal_setup_failed(exec_ready_fd, "pivot_root failed"); // WP-#104
                unsafe { libc::_exit(1) };
            }

            // chdir to new root
            if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
                eprintln!("lightr-engine ns: chdir / failed");
                signal_setup_failed(exec_ready_fd, "chdir / failed"); // WP-#104
                unsafe { libc::_exit(1) };
            }

            // #91: give the container a minimal /dev. The CAS-materialized rootfs has
            // an EMPTY /dev (snapshot carries file content, not device nodes), so
            // programs that need /dev/null (e.g. any shell job-control `cmd &`),
            // /dev/zero, /dev/urandom, … fail. Rootless cannot `mknod` (no CAP_MKNOD in
            // the init userns), so we BIND-mount the host's device nodes — still
            // reachable at /.put_old/dev/* until put_old is unmounted just below. A
            // tmpfs at /dev gives a clean, writable surface for the bind targets
            // without mutating the rootfs. Best-effort: a device we can't wire is
            // skipped (a missing optional node must not fail an otherwise-good run;
            // pre-#91 there was no /dev at all).
            setup_minimal_dev();

            // #92: mount a tmpfs at /dev/shm (Docker's POSIX shared-memory mount). The
            // CAS-materialized rootfs has none, so programs that need /dev/shm (e.g.
            // Python multiprocessing, many DB clients) otherwise fail. /dev is the
            // tmpfs from setup_minimal_dev, so the /dev/shm dir is created there. A
            // default 64 MiB mount (Docker's default) is best-effort; an EXPLICIT
            // `--shm-size` that cannot be applied is fail-closed (`_exit(1)`) — a
            // requested size silently dropped would be a parity lie.
            let shm_bytes = shm_size.unwrap_or(64 * 1024 * 1024);
            if let Err(e) = setup_shm(shm_bytes, shm_size.is_some()) {
                eprintln!("lightr-engine ns: /dev/shm mount failed: {e}");
                signal_setup_failed(exec_ready_fd, "/dev/shm mount failed"); // WP-#104
                unsafe { libc::_exit(1) };
            }

            // `--tmpfs` (Docker parity): mount a fresh tmpfs at each requested target.
            // Done AFTER /dev/shm and BEFORE the rootfs read-only remount, so each
            // tmpfs is an independent submount (the NON-recursive RO remount of `/`
            // leaves it writable — same property as /dev/shm). We are POST-pivot, so a
            // target is `/<target>` in the new root (mirrors the bind/shm targets).
            // `MS_NOSUID|MS_NODEV` matches Docker's `--tmpfs` default (exec ALLOWED — no
            // MS_NOEXEC). Fail-closed: a requested tmpfs that cannot be mounted
            // `_exit`s rather than exec without it. Empty ⇒ no-op (pre-feature path).
            for t in tmpfs {
                if let Err(e) = setup_tmpfs(t) {
                    eprintln!("lightr-engine ns: tmpfs {:?} mount failed: {e}", t.target);
                    signal_setup_failed(exec_ready_fd, "tmpfs mount failed");
                    unsafe { libc::_exit(1) };
                }
            }

            // Unmount put_old (AFTER /dev binds + the proc mount are established;
            // MNT_DETACH is lazy so the already-bound mounts — including /proc — survive).
            let inner_put_old = CString::new("/.put_old").unwrap();
            let _ = unsafe { libc::umount2(inner_put_old.as_ptr(), libc::MNT_DETACH) };

            // #92: `--read-only` ⇒ remount the rootfs READ-ONLY. Done LAST — after the
            // /proc + /dev + /dev/shm mounts — and NON-recursively, so only the `/`
            // mount (the rootfs bind) flips to RO; the /proc + /dev + /dev/shm
            // SUBMOUNTS are independent mount points and keep their flags. Net effect:
            // rootfs immutable, /proc + /dev + /dev/shm intact (the key correctness
            // point — a container with a RO root still needs a live /proc + writable
            // shared memory). Fail-closed: if the remount fails we `_exit(1)` rather
            // than exec a writable root the user asked to be read-only.
            if read_only {
                if let Err(e) = remount_root_readonly() {
                    eprintln!("lightr-engine ns: read-only remount failed: {e}");
                    signal_setup_failed(exec_ready_fd, "read-only remount failed"); // WP-#104
                    unsafe { libc::_exit(1) };
                }
            }

            // chdir to cwd-within-rootfs, or fallback to /
            let cwd_in = if cwd.is_empty() { "/" } else { cwd };
            let cwd_c = match CString::new(cwd_in.as_bytes()) {
                Ok(c) => c,
                Err(_) => CString::new("/").unwrap(),
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
            let prog_resolved = match crate::pathres::resolve_in_path(&command[0], env_path) {
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
            };

            if init {
                // `--init`: PID 1 is a minimal reaper; the workload is its child (so
                // the workload is PID 2). PID 1 reaps orphaned zombies and propagates
                // the workload's exit code.
                let child = unsafe { libc::fork() };
                if child < 0 {
                    eprintln!(
                        "lightr-engine ns: init workload fork failed: {}",
                        std::io::Error::last_os_error()
                    );
                    signal_setup_failed(exec_ready_fd, "init workload fork failed"); // WP-#104
                    unsafe { libc::_exit(1) };
                }
                if child == 0 {
                    // ── great-grandchild: the workload (PID 2) ──
                    // `--ulimit`: apply per-process `setrlimit` caps EARLY — BEFORE
                    // caps/user/seccomp, so a hard-limit RAISE still holds
                    // CAP_SYS_RESOURCE (a lowering always works). Fail-closed.
                    apply_ulimits_if_any(ulimits, exec_ready_fd);
                    // Caps applied LAST, in the execing process (fail-closed: a capset
                    // failure `_exit`s rather than exec with the WRONG set).
                    apply_caps_if_any(desired_caps_vec.as_deref(), exec_ready_fd);
                    // WP-#106: apply the AppArmor profile LAST (after caps), right
                    // before execv (aa_change_onexec). Fail-closed: a profile that
                    // can't be applied `_exit`s rather than exec unconfined.
                    apply_apparmor_if_any(apparmor, exec_ready_fd);
                    // `--user`: drop to the target uid/gid AFTER caps/apparmor (which
                    // need privilege) but BEFORE seccomp — the switch must run while we
                    // still hold CAP_SETUID/SETGID and before any filter could block the
                    // setuid syscalls. `execve` then naturally clears caps for the now
                    // non-root process. Fail-closed. `None` ⇒ no-op.
                    apply_user_if_any(user, exec_ready_fd);
                    // WP-#108: install the (pre-compiled) seccomp filter LAST — after
                    // apparmor, right before execv. Fail-closed: an install failure
                    // `_exit`s rather than exec unfiltered. `None` (no profile /
                    // "unconfined") ⇒ no-op.
                    apply_seccomp_if_any(compiled_seccomp.as_ref(), exec_ready_fd);
                    // WP-#102: arm the exec-success pipe (CLOEXEC) right before execv.
                    arm_exec_ready(exec_ready_fd);
                    // PATH-resolved program (argv unchanged ⇒ argv[0] stays the
                    // conventional name, matching execvp).
                    unsafe { libc::execv(prog_resolved.as_ptr(), argv_ptrs.as_ptr()) };
                    // execv returned ⇒ it FAILED. Signal BYTES down the pipe first.
                    let e = std::io::Error::last_os_error();
                    signal_exec_failed(exec_ready_fd, &e);
                    eprintln!("lightr-engine ns: exec failed: {e}");
                    unsafe { libc::_exit(127) };
                }
                // WP-#102: PID 1 (the reaper) must CLOSE its copy of the write end now
                // that the workload (the only legitimate holder) is forked — otherwise
                // EOF would wait for the WHOLE container to exit, not the workload's
                // execv. `--init` only; CRI uses init=false (correctness-for-completeness).
                if let Some(fd) = exec_ready_fd {
                    unsafe { libc::close(fd) };
                }
                // PID 1 reaper loop — never returns (always `_exit`s).
                reaper_loop(child);
            } else {
                // No `--init`: the workload itself is PID 1.
                // `--ulimit`: apply per-process `setrlimit` caps EARLY — BEFORE
                // caps/user/seccomp (a hard-limit raise still holds CAP_SYS_RESOURCE;
                // a lowering always works). Fail-closed.
                apply_ulimits_if_any(ulimits, exec_ready_fd);
                // Caps LAST, in the execing process (fail-closed via `_exit`).
                apply_caps_if_any(desired_caps_vec.as_deref(), exec_ready_fd);
                // WP-#106: apply the AppArmor profile LAST (after caps), right before
                // execv (aa_change_onexec). Fail-closed: a profile that can't be
                // applied `_exit`s rather than exec unconfined.
                apply_apparmor_if_any(apparmor, exec_ready_fd);
                // `--user`: drop to the target uid/gid AFTER caps/apparmor (which need
                // privilege) but BEFORE seccomp — the switch must run while we still
                // hold CAP_SETUID/SETGID and before any filter could block the setuid
                // syscalls. `execve` then naturally clears caps for the now non-root
                // process. Fail-closed. `None` ⇒ no-op.
                apply_user_if_any(user, exec_ready_fd);
                // WP-#108: install the (pre-compiled) seccomp filter LAST — after
                // apparmor, right before execv. Fail-closed: an install failure
                // `_exit`s rather than exec unfiltered. `None` (no profile /
                // "unconfined") ⇒ no-op.
                apply_seccomp_if_any(compiled_seccomp.as_ref(), exec_ready_fd);
                // WP-#102: arm the exec-success pipe (CLOEXEC) right before execv — a
                // successful execv auto-closes it ⇒ the backend's reader sees EOF.
                arm_exec_ready(exec_ready_fd);
                // PATH-resolved program (argv unchanged ⇒ argv[0] stays the
                // conventional name, matching execvp).
                unsafe { libc::execv(prog_resolved.as_ptr(), argv_ptrs.as_ptr()) };
                // execv returned ⇒ it FAILED. Signal BYTES down the pipe first.
                let e = std::io::Error::last_os_error();
                signal_exec_failed(exec_ready_fd, &e);
                eprintln!("lightr-engine ns: exec failed: {e}");
                unsafe { libc::_exit(127) };
            }
        }

        // ── setup process (NOT in the new pid ns; external parent of PID 1) ──────
        // Wait for PID 1 (the grandchild) and propagate its code to `run()`'s parent.
        // WP-#102: the setup process still holds a copy of the pipe write end
        // (inherited through both forks). Close it NOW — before the blocking waitpid —
        // so ONLY PID 1 holds the write end and EOF fires on PID 1's successful execv,
        // not when the container finally exits (THE #1 EOF-never-fires risk).
        if let Some(fd) = exec_ready_fd {
            unsafe { libc::close(fd) };
        }
        let mut wstatus: libc::c_int = 0;
        let r = unsafe { libc::waitpid(workload_pid, &mut wstatus, 0) };
        if r == -1 {
            eprintln!(
                "lightr-engine ns: waitpid(pid1) failed: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }
        wait_to_exit_code(wstatus)
    }

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
    fn apply_ulimits_if_any(ulimits: &[Ulimit], exec_ready_fd: Option<libc::c_int>) {
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

    fn apply_caps_if_any(desired: Option<&[u32]>, exec_ready_fd: Option<libc::c_int>) {
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
    fn apply_apparmor_if_any(profile: Option<&str>, exec_ready_fd: Option<libc::c_int>) {
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
    fn apply_seccomp_if_any(
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

    /// `--user`: drop the EXECing process to the requested uid/gid. Called post-fork,
    /// AFTER caps/apparmor and BEFORE seccomp — at this point we are post-pivot (so
    /// `/etc/passwd`/`/etc/group` are the CONTAINER files) and still hold
    /// CAP_SETUID/CAP_SETGID from the userns baseline. `None` ⇒ no-op (byte-identical
    /// to the pre-feature path). Like the other PID-1 setup steps this is FAIL-CLOSED:
    /// an unresolvable/malformed spec or any failing setgroups/setgid/setuid signals
    /// the exec-readiness pipe with bytes (so the kernel-closed fd is NOT misread as
    /// EOF ⇒ a false `Running`) and `_exit(1)`s rather than exec with the WRONG identity
    /// (running the workload as root when a non-root user was requested is a SECURITY
    /// bug, worse than an error).
    fn apply_user_if_any(user: Option<&str>, exec_ready_fd: Option<libc::c_int>) {
        let spec = match user {
            None => return,
            Some(s) => s,
        };
        let (uid, gid) = match resolve_user(spec) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("lightr-engine ns: --user: {e}");
                signal_setup_failed(exec_ready_fd, &format!("--user: {e}"));
                unsafe { libc::_exit(1) };
            }
        };
        // v1 SCOPE (honest boundary): the ns userns uses a SINGLE-uid map
        // (`"0 <outer> 1"`) with `setgroups=deny` (see the uid_map/gid_map writes
        // above), so the ONLY identity that exists inside the container is
        // container-root (uid/gid 0). A switch to ANY other uid/gid would EPERM (the
        // target is unmapped, and setgroups is forbidden). Therefore:
        //   - `(0,0)` (root / `--user 0`) ⇒ NO-OP (already root; we must NOT call
        //     setgroups — it is denied — so just return).
        //   - any NON-root target ⇒ HONEST-ERROR (this REPLACES the prior SILENT
        //     run-as-root, a security footgun: a workload that asked for uid 1000 must
        //     never silently run as root). Real non-root support needs a rootless
        //     subuid RANGE mapping (newuidmap + /etc/subuid) so the target uid exists
        //     inside the userns — tracked as #115.
        if (uid, gid) == (0, 0) {
            return;
        }
        eprintln!(
            "lightr-engine ns: --user {spec:?} (uid={uid} gid={gid}): running as a \
             non-root in-container user is not yet supported on the rootless ns engine \
             (its single-uid userns maps only container-root; a subuid RANGE mapping is \
             required) — run as root, or use --engine native (host-mapped uid/gid)"
        );
        signal_setup_failed(
            exec_ready_fd,
            "--user: non-root uid/gid not supported on the ns engine (subuid range required)",
        );
        unsafe { libc::_exit(1) };
    }

    /// Resolve a `--user` spec (`uid[:gid]` or `name[:group]`) to numeric `(uid, gid)`
    /// against the CONTAINER `/etc/passwd`/`/etc/group` (we are post-pivot, so the bare
    /// paths are the container's). Resolution rules (Docker parity):
    ///   - uid part all-digits ⇒ parse u32; else a NAME ⇒ look it up in `/etc/passwd`
    ///     (`name:passwd:uid:gid:...`), taking the uid AND its primary gid.
    ///   - gid part present + all-digits ⇒ parse; present + a NAME ⇒ look it up in
    ///     `/etc/group` (`name:passwd:gid:...`).
    ///   - gid ABSENT: numeric uid ⇒ gid 0 (Docker: `--user 1000` ⇒ gid 0); NAME uid ⇒
    ///     the primary gid from that user's `/etc/passwd` entry.
    /// Any unresolvable name / malformed value ⇒ `Err` (caller fails closed).
    fn resolve_user(spec: &str) -> std::result::Result<(u32, u32), String> {
        if spec.is_empty() {
            return Err("empty user spec".to_string());
        }
        let (uid_part, gid_part) = match spec.split_once(':') {
            Some((u, g)) => (u, Some(g)),
            None => (spec, None),
        };
        if uid_part.is_empty() {
            return Err(format!("malformed user spec {spec:?}"));
        }

        // Resolve the uid part. `passwd_gid` is the user's primary gid from
        // /etc/passwd, used only when the gid part is absent AND the uid was a name.
        let (uid, passwd_gid): (u32, Option<u32>) = if uid_part.bytes().all(|b| b.is_ascii_digit())
        {
            let uid = uid_part
                .parse::<u32>()
                .map_err(|_| format!("invalid numeric uid {uid_part:?}"))?;
            (uid, None)
        } else {
            let (uid, pgid) = passwd_lookup(uid_part)
                .map_err(|e| format!("reading /etc/passwd: {e}"))?
                .ok_or_else(|| format!("user {uid_part:?} not found in container /etc/passwd"))?;
            (uid, Some(pgid))
        };

        // Resolve the gid part.
        let gid: u32 = match gid_part {
            Some(g) if g.is_empty() => return Err(format!("malformed user spec {spec:?}")),
            Some(g) if g.bytes().all(|b| b.is_ascii_digit()) => g
                .parse::<u32>()
                .map_err(|_| format!("invalid numeric gid {g:?}"))?,
            Some(g) => group_lookup(g)
                .map_err(|e| format!("reading /etc/group: {e}"))?
                .ok_or_else(|| format!("group {g:?} not found in container /etc/group"))?,
            // gid absent: numeric uid ⇒ 0 (Docker default); name uid ⇒ its primary gid.
            None => passwd_gid.unwrap_or(0),
        };

        Ok((uid, gid))
    }

    /// Look up a user NAME in the container `/etc/passwd`. Returns `(uid, primary_gid)`
    /// or `None` if absent. Lines: `name:passwd:uid:gid:gecos:home:shell`. Malformed /
    /// non-numeric id fields are skipped (lenient, matching libc getpwnam). A missing
    /// file is an I/O error ⇒ fail-closed.
    fn passwd_lookup(name: &str) -> std::io::Result<Option<(u32, u32)>> {
        let content = std::fs::read_to_string("/etc/passwd")?;
        for line in content.lines() {
            let mut f = line.split(':');
            if f.next() != Some(name) {
                continue;
            }
            let _passwd = f.next();
            let uid = match f.next().and_then(|s| s.parse::<u32>().ok()) {
                Some(u) => u,
                None => continue,
            };
            let gid = match f.next().and_then(|s| s.parse::<u32>().ok()) {
                Some(g) => g,
                None => continue,
            };
            return Ok(Some((uid, gid)));
        }
        Ok(None)
    }

    /// Look up a group NAME in the container `/etc/group`. Returns the gid or `None` if
    /// absent. Lines: `name:passwd:gid:members`. A missing file is an I/O error ⇒
    /// fail-closed.
    fn group_lookup(name: &str) -> std::io::Result<Option<u32>> {
        let content = std::fs::read_to_string("/etc/group")?;
        for line in content.lines() {
            let mut f = line.split(':');
            if f.next() != Some(name) {
                continue;
            }
            let _passwd = f.next();
            if let Some(gid) = f.next().and_then(|s| s.parse::<u32>().ok()) {
                return Ok(Some(gid));
            }
        }
        Ok(None)
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

    /// WP-#102: arm the exec-readiness pipe for a SUCCESSFUL exec by setting the
    /// write end `FD_CLOEXEC`. A successful `execv` then makes the kernel auto-close
    /// it ⇒ the backend's reader sees EOF ⇒ the workload is actually running. Called
    /// in the EXECing process immediately before `execv`. No-op when no pipe is wired
    /// (`None` ⇒ byte-identical to the pre-#102 path). Best-effort: a failed `fcntl`
    /// at worst leaves the fd open so EOF waits for exit — not a false `Running`.
    fn arm_exec_ready(fd: Option<libc::c_int>) {
        if let Some(fd) = fd {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }

    /// WP-#102: signal an `execv` FAILURE down the exec-readiness pipe by WRITING the
    /// error bytes (the reader distinguishes BYTES ⇒ start failed from EOF ⇒ success).
    /// Called AFTER `execv` returns (i.e. it failed) and BEFORE `_exit(127)`. No-op
    /// when no pipe is wired. The write is best-effort (raw libc; we are post-fork).
    fn signal_exec_failed(fd: Option<libc::c_int>, err: &std::io::Error) {
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
    fn signal_setup_failed(fd: Option<libc::c_int>, msg: &str) {
        if let Some(fd) = fd {
            let line = format!("setup failed: {msg}");
            unsafe {
                libc::write(fd, line.as_ptr() as *const libc::c_void, line.len());
            }
        }
    }

    /// WP-#96: mount a fresh `proc` at `target` (`<rootfs>/proc`) from PID 1, BEFORE
    /// pivot_root. Two conditions make a rootless fresh procfs mount legal and correct,
    /// and BOTH hold only at this call site. First, the host `/proc` (inherited via
    /// CLONE_NEWNS) is STILL fully-visible, so the kernel's
    /// `mount_too_revealing`/`fs_fully_visible` check passes (post-pivot it would be
    /// gone → EPERM, the #95 bug this WP fixes). Second, the caller is PID 1 in the NEW
    /// pid namespace, so the mounted proc reflects that ns (`cat /proc/self/status` ⇒
    /// `Pid: 1`). Best-effort: mkdir the target first (the CAS rootfs may lack `/proc`);
    /// if the mount fails we log + continue (it should now succeed; CI hard-requires
    /// it). `MS_NOSUID|MS_NODEV|MS_NOEXEC` matches the runc/youki hardening for `/proc`.
    fn mount_proc(target: &std::path::Path) {
        use std::ffi::CString;
        let _ = std::fs::create_dir_all(target);
        let tgt = match CString::new(target.as_os_str().as_encoded_bytes()) {
            Ok(c) => c,
            Err(_) => return,
        };
        let (src, fstype) = (CString::new("proc").unwrap(), CString::new("proc").unwrap());
        let r = unsafe {
            libc::mount(
                src.as_ptr(),
                tgt.as_ptr(),
                fstype.as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                std::ptr::null(),
            )
        };
        if r != 0 {
            eprintln!(
                "lightr-engine ns: /proc mount failed (continuing): {}",
                std::io::Error::last_os_error()
            );
        }
    }

    /// WP-#95 (`--init`): the minimal PID-1 reaper loop. Blocks in `waitpid(-1)`,
    /// reaping every child (orphaned grandchildren re-parent to PID 1). When the
    /// tracked `workload_child` exits we record its code, drain any already-exited
    /// remaining children (non-blocking — we don't wait on long-lived orphans), then
    /// `_exit` with the workload's code so the run's exit status is the workload's.
    /// `ECHILD` (no children left) also exits. `EINTR`/other transient errors retry.
    /// Raw libc only (post-fork, pre-`_exit`): no allocation in the loop body.
    fn reaper_loop(workload_child: libc::pid_t) -> ! {
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

    fn write_map(path: &str, content: &str) -> std::io::Result<()> {
        std::fs::write(path, content.as_bytes())
    }

    /// WP-#99: open the pinned netns at `path` `O_RDONLY` and `setns` into it
    /// (CLONE_NEWNET). MUST be called while still real root in the HOST init userns
    /// (BEFORE `unshare(CLONE_NEWUSER)`) — see the call-site ordering note. Returns
    /// an honest `io::Error` on open/setns failure so the caller fails closed.
    fn setns_netns(path: &std::path::Path) -> std::io::Result<()> {
        use std::os::unix::io::AsRawFd;
        let f = std::fs::OpenOptions::new().read(true).open(path)?;
        let rc = unsafe { libc::setns(f.as_raw_fd(), libc::CLONE_NEWNET) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(()) // `f` drops here, closing the fd after the successful setns.
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

    // WP-NET-ISO: bring the loopback interface up inside the new netns. Opens a
    // DGRAM socket, reads `lo`'s flags (SIOCGIFFLAGS), ORs in IFF_UP|IFF_RUNNING,
    // writes them back (SIOCSIFFLAGS), and closes the socket. Returns an honest
    // io::Error on any failure (the caller fails closed). We define a minimal
    // `ifreq` whose first union member is the 16-bit flags field — the only field
    // these two ioctls touch — laid out to match the C `struct ifreq`.
    fn bring_up_loopback() -> std::io::Result<()> {
        // `struct ifreq` on Linux: char ifr_name[IFNAMSIZ=16] followed by a union
        // whose largest member is 16 bytes (sockaddr/etc.). For the FLAGS ioctls
        // only `ifr_flags` (a `short` at the start of the union) is read/written.
        #[repr(C)]
        struct IfReq {
            ifr_name: [libc::c_char; libc::IFNAMSIZ],
            ifr_flags: libc::c_short,
            // Pad the union out to its full size so the struct matches the kernel's
            // `struct ifreq` layout (the ioctls copy a full ifreq in/out).
            _pad: [u8; 22],
        }

        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut req = IfReq {
            ifr_name: [0; libc::IFNAMSIZ],
            ifr_flags: 0,
            _pad: [0; 22],
        };
        // Copy "lo" into ifr_name (NUL-terminated; the array is zero-initialized).
        let lo = b"lo";
        for (i, &b) in lo.iter().enumerate() {
            req.ifr_name[i] = b as libc::c_char;
        }

        // SIOCGIFFLAGS: read current flags.
        if unsafe { libc::ioctl(fd, libc::SIOCGIFFLAGS, &mut req) } != 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }

        // OR in IFF_UP | IFF_RUNNING.
        req.ifr_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;

        // SIOCSIFFLAGS: write them back.
        if unsafe { libc::ioctl(fd, libc::SIOCSIFFLAGS, &req) } != 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }

        unsafe { libc::close(fd) };
        Ok(())
    }

    /// #91: populate the container's /dev with the standard device nodes by
    /// BIND-mounting the host's (rootless cannot `mknod`). Called after pivot_root
    /// + `chdir /` but BEFORE `/.put_old` is unmounted, while the host nodes are
    /// still reachable at `/.put_old/dev/*`. A fresh tmpfs at /dev gives a clean,
    /// writable surface for the bind targets without mutating the rootfs. Entirely
    /// best-effort: any step that fails is skipped (a device we can't wire must not
    /// fail an otherwise-good run — pre-#91 there was no /dev at all).
    fn setup_minimal_dev() {
        use std::ffi::CString;
        let _ = std::fs::create_dir_all("/dev");
        // Fresh tmpfs at /dev (tmpfs is mountable in a user namespace).
        if let (Ok(src), Ok(tgt), Ok(fstype), Ok(opts)) = (
            CString::new("tmpfs"),
            CString::new("/dev"),
            CString::new("tmpfs"),
            CString::new("mode=0755"),
        ) {
            unsafe {
                libc::mount(
                    src.as_ptr(),
                    tgt.as_ptr(),
                    fstype.as_ptr(),
                    libc::MS_NOSUID,
                    opts.as_ptr() as *const libc::c_void,
                );
            }
        }
        // Bind each standard node from the still-mounted old root.
        for name in ["null", "zero", "full", "random", "urandom", "tty"] {
            let src = format!("/.put_old/dev/{name}");
            let dst = format!("/dev/{name}");
            if std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&dst)
                .is_err()
            {
                continue;
            }
            if let (Ok(s), Ok(d)) = (CString::new(src), CString::new(dst)) {
                unsafe {
                    libc::mount(
                        s.as_ptr(),
                        d.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND,
                        std::ptr::null(),
                    );
                }
            }
        }
        // #105: give the container a PRIVATE devpts at /dev/pts (Docker mounts one).
        // The /dev tmpfs above is writable, so mkdir the mountpoint, then mount a
        // `devpts` with `newinstance` — a private pts namespace, isolated from the
        // host's (and any other container's) ptys. `ptmxmode=0666` makes the ptmx
        // node world-rw so unprivileged programs can allocate a pty WITHOUT relying on
        // the `gid=5` (tty group) option: that gid does NOT map inside the container's
        // user namespace (rootless/userns), so passing it would EINVAL — we omit gid
        // by design and rely on ptmxmode. MS_NOSUID|MS_NOEXEC matches Docker/runc
        // hardening. Mountable here because PID 1 holds CAP_SYS_ADMIN in the new
        // user+mount ns. Entirely best-effort (eprintln on failure): #103 `exec -it`
        // already works via isatty WITHOUT a devpts, so a devpts that won't mount must
        // NOT kill the container — it only adds the /dev/pts NAME surface.
        let _ = std::fs::create_dir_all("/dev/pts");
        if let (Ok(src), Ok(tgt), Ok(fstype), Ok(opts)) = (
            CString::new("devpts"),
            CString::new("/dev/pts"),
            CString::new("devpts"),
            CString::new("newinstance,ptmxmode=0666"),
        ) {
            let r = unsafe {
                libc::mount(
                    src.as_ptr(),
                    tgt.as_ptr(),
                    fstype.as_ptr(),
                    libc::MS_NOSUID | libc::MS_NOEXEC,
                    opts.as_ptr() as *const libc::c_void,
                )
            };
            if r != 0 {
                eprintln!(
                    "lightr-engine ns: /dev/pts devpts mount failed (continuing): {}",
                    std::io::Error::last_os_error()
                );
            } else {
                // #105: /dev/ptmx → pts/ptmx (Docker's layout). With `newinstance` the
                // multiplexor lives at /dev/pts/ptmx; a relative symlink is the simplest
                // rootless-safe way to expose it at the conventional /dev/ptmx path.
                let _ = std::os::unix::fs::symlink("pts/ptmx", "/dev/ptmx");
            }
        }
        // Convenience symlinks programs expect (harmless if /proc isn't mounted —
        // creating the link always succeeds; only following it would need /proc).
        let _ = std::os::unix::fs::symlink("/proc/self/fd", "/dev/fd");
        let _ = std::os::unix::fs::symlink("/proc/self/fd/0", "/dev/stdin");
        let _ = std::os::unix::fs::symlink("/proc/self/fd/1", "/dev/stdout");
        let _ = std::os::unix::fs::symlink("/proc/self/fd/2", "/dev/stderr");
    }

    /// #92: mount a tmpfs at `/dev/shm` sized to `bytes` (`mode=1777`, Docker's
    /// `/dev/shm`). `/dev` is the tmpfs from [`setup_minimal_dev`], so the dir is
    /// created there first. `explicit` is true for a user `--shm-size`: such a
    /// mount is fail-closed (an `Err` is returned, the run aborts) — a requested
    /// size silently dropped is a parity lie. The default 64 MiB mount
    /// (`explicit=false`) is best-effort: `/dev/shm` should always exist, but a
    /// default that cannot mount must not fail an otherwise-good run.
    fn setup_shm(bytes: u64, explicit: bool) -> std::io::Result<()> {
        use std::ffi::CString;
        let _ = std::fs::create_dir_all("/dev/shm");
        let opts = format!("mode=1777,size={bytes}");
        let (src, tgt, fstype, opts_c) = match (
            CString::new("tmpfs"),
            CString::new("/dev/shm"),
            CString::new("tmpfs"),
            CString::new(opts),
        ) {
            (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
            _ => return Err(std::io::Error::other("bad /dev/shm mount arg")),
        };
        let r = unsafe {
            libc::mount(
                src.as_ptr(),
                tgt.as_ptr(),
                fstype.as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                opts_c.as_ptr() as *const libc::c_void,
            )
        };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            if explicit {
                return Err(e);
            }
            // Best-effort default: note it and continue (no /dev/shm is degraded,
            // not fatal, for a run that did not request a specific size).
            eprintln!("lightr-engine ns: default /dev/shm tmpfs mount failed (continuing): {e}");
        }
        Ok(())
    }

    /// `--tmpfs` (Docker parity): mount a fresh tmpfs at `t.target` (POST-pivot, so
    /// `/<target>` in the new root). Mirrors `setup_shm`'s `libc::mount` shape: same
    /// `MS_NOSUID|MS_NODEV` flags (exec ALLOWED — Docker's `--tmpfs` default; NO
    /// MS_NOEXEC) and the same `mode=...,size=...` option string (size omitted when
    /// `None` ⇒ the kernel default). `mkdir -p`s the target first (the image may lack
    /// it). Fail-closed: any error is returned so the caller `_exit`s (a requested
    /// tmpfs silently dropped would be a parity lie).
    fn setup_tmpfs(t: &TmpfsMount) -> std::io::Result<()> {
        use std::ffi::CString;
        // Strip a leading '/' so a POST-pivot absolute target stays the in-root path
        // when joined to "/" (an empty/relative target falls back to the verbatim
        // value); the mount target itself is the absolute container path.
        let target = &t.target;
        std::fs::create_dir_all(target)?;
        // Mode is always present (defaulted to 1777 by the CLI); size only when set.
        let opts = match t.size {
            Some(bytes) => format!("mode={},size={}", t.mode, bytes),
            None => format!("mode={}", t.mode),
        };
        let (src, tgt, fstype, opts_c) = match (
            CString::new("tmpfs"),
            CString::new(target.as_bytes()),
            CString::new("tmpfs"),
            CString::new(opts),
        ) {
            (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
            _ => return Err(std::io::Error::other("bad tmpfs mount arg")),
        };
        let r = unsafe {
            libc::mount(
                src.as_ptr(),
                tgt.as_ptr(),
                fstype.as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                opts_c.as_ptr() as *const libc::c_void,
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// WP-#107 (CRI GAP 2/3): write `content` to `<rootfs>/<rel>` (e.g.
    /// `etc/resolv.conf`, `etc/hostname`) from PID 1 BEFORE pivot_root. `mkdir -p`s
    /// the parent (`etc/`) first — the CAS rootfs may lack it. Returns an honest
    /// `io::Error` so the caller fails closed. Uses std file I/O, consistent with the
    /// other PID-1 setup steps (`create_dir_all`, `symlink`); the child is
    /// single-threaded post-fork.
    fn write_rootfs_file(
        rootfs: &std::path::Path,
        rel: &str,
        content: &[u8],
    ) -> std::io::Result<()> {
        let target = rootfs.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, content)
    }

    /// `--add-host`: APPEND `content` to `<rootfs>/<rel>` (e.g. `etc/hosts`) from PID
    /// 1 BEFORE pivot_root, preserving any existing content (the image's
    /// `127.0.0.1 localhost`). `mkdir -p`s the parent (`etc/`) and CREATEs the file
    /// if missing — mirrors `write_rootfs_file` but opens append-or-create instead of
    /// truncating. Returns an honest `io::Error` so the caller fails closed.
    fn append_rootfs_file(
        rootfs: &std::path::Path,
        rel: &str,
        content: &[u8],
    ) -> std::io::Result<()> {
        use std::io::Write;
        let target = rootfs.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)?;
        f.write_all(content)
    }

    // WP-#112: hermetic proof that `--add-host` APPENDS to /etc/hosts (preserves the
    // image's existing content) rather than truncating it — the base-image-independent
    // version of the CI /etc/hosts check (the alpine probe rootfs ships no `localhost`
    // line, so this property must be pinned with controlled inputs).
    #[cfg(test)]
    mod hosts_tests {
        use super::append_rootfs_file;

        #[test]
        fn append_rootfs_file_preserves_existing_then_appends() {
            let tmp = std::env::temp_dir().join(format!("lightr-hosts-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(tmp.join("etc")).unwrap();
            // Seed an image-style /etc/hosts.
            std::fs::write(tmp.join("etc/hosts"), b"127.0.0.1\tlocalhost\n").unwrap();
            // --add-host adds one line.
            append_rootfs_file(&tmp, "etc/hosts", b"10.9.8.7\tmyhost.local\n").unwrap();
            let got = std::fs::read_to_string(tmp.join("etc/hosts")).unwrap();
            assert!(got.contains("127.0.0.1\tlocalhost"), "existing line preserved");
            assert!(got.contains("10.9.8.7\tmyhost.local"), "new line appended");
            // order: existing first, appended after (no truncation).
            assert!(
                got.find("localhost").unwrap() < got.find("myhost.local").unwrap(),
                "append, not prepend/truncate"
            );
            let _ = std::fs::remove_dir_all(&tmp);
        }

        #[test]
        fn append_rootfs_file_creates_when_missing() {
            let tmp = std::env::temp_dir().join(format!("lightr-hosts2-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).unwrap();
            // No pre-existing /etc/hosts (alpine-style): the helper creates it + parent.
            append_rootfs_file(&tmp, "etc/hosts", b"10.9.8.7\tmyhost.local\n").unwrap();
            let got = std::fs::read_to_string(tmp.join("etc/hosts")).unwrap();
            assert_eq!(got, "10.9.8.7\tmyhost.local\n");
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }

    /// WP-#107 (CRI GAP 1): bind-mount one CRI volume into the rootfs BEFORE
    /// pivot_root. The target is `<rootfs>/<container_path>` (leading `/` stripped so
    /// `join` stays inside the rootfs); `mkdir -p` it first (the image may lack it).
    /// The source `host_path` is already host-side realpath'd in build_ns_plan (the
    /// symlink-host-path spec), so we bind it verbatim. `MS_BIND|MS_REC` mounts the
    /// dir; when `readonly`, a second `MS_BIND|MS_REMOUNT|MS_RDONLY` makes it RO (the
    /// canonical two-step). Returns an honest `io::Error` so the caller fails closed.
    fn apply_bind_mount(
        rootfs: &std::path::Path,
        m: &BindMount,
    ) -> std::io::Result<()> {
        use std::ffi::CString;
        // Strip a leading '/' so `container_path` joins INSIDE the rootfs.
        let rel = m.container_path.trim_start_matches('/');
        let target = rootfs.join(rel);
        std::fs::create_dir_all(&target)?;

        let src = CString::new(m.host_path.as_bytes())
            .map_err(|_| std::io::Error::other("bind mount host_path has interior NUL"))?;
        let tgt = CString::new(target.as_os_str().as_encoded_bytes())
            .map_err(|_| std::io::Error::other("bind mount target has interior NUL"))?;

        let r = unsafe {
            libc::mount(
                src.as_ptr(),
                tgt.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }

        if m.readonly {
            let r = unsafe {
                libc::mount(
                    std::ptr::null(),
                    tgt.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                    std::ptr::null(),
                )
            };
            if r != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// #92: remount `/` (the pivoted rootfs bind) READ-ONLY. NON-recursive on
    /// purpose: it flips ONLY the `/` mount, leaving the /dev + /dev/shm tmpfs
    /// submounts (independent mount points) writable. `MS_BIND | MS_REMOUNT |
    /// MS_RDONLY` is the canonical incantation to make a bind mount read-only; it
    /// works rootless because we hold CAP_SYS_ADMIN in the new user+mount ns.
    fn remount_root_readonly() -> std::io::Result<()> {
        let r = unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
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
        assert_eq!(normalize_cap("  cap_net_bind_service  "), "NET_BIND_SERVICE");
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
        assert_eq!(d, vec![10], "--cap-drop ALL --cap-add NET_BIND_SERVICE ⇒ {{10}}");
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
