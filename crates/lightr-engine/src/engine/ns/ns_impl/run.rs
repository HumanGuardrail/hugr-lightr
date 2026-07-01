//! ns_impl::run — `run_in_namespaces`: the container launch core. The netns
//! join, the user+mount+pid(+uts/net) unshare, the uid/gid map (single-uid or
//! subid RANGE handshake), cgroup apply, loopback bring-up, cap PARSE, the
//! pid-ns fork, then PID-1 rootfs setup (delegated to `rootfs.rs`) and the
//! apply+exec tail (delegated to `apply.rs`). All items live inside the
//! Linux-gated `ns_impl` module (see `ns/mod.rs`).

use super::apply::{apply_and_exec, chdir_and_resolve};
use super::mounts::{bring_up_loopback, setns_netns, write_map};
use super::rootfs::setup_rootfs_and_pivot;
use super::signal::{reaper_loop, signal_setup_failed, wait_to_exit_code};
use crate::engine::ns::desired_caps;
use crate::engine::spec::{BindMount, TmpfsMount, Ulimit};
use std::ffi::CString;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_in_namespaces(
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
    // `--oom-score-adj`: written to /proc/self/oom_score_adj in PID 1, EARLY
    // (alongside the ulimits apply). `None` ⇒ the pre-feature path is unchanged.
    oom_score_adj: Option<i32>,
    // WP-#114: the CHILD-side ends `(ready_w, done_r)` of the subuid RANGE-map
    // handshake. `Some` ⇒ a real non-root `--user` with subid support: skip the
    // single-uid self-map, signal "userns created" on `ready_w`, wait for the
    // outside parent's newuidmap/newgidmap on `done_r`, and (in PID 1) do a REAL
    // privilege drop (the target uid IS mapped). `None` ⇒ the byte-identical
    // single-uid path (root no-op / non-root #113 honest-error).
    subid_sync: Option<(libc::c_int, libc::c_int)>,
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

    // Map the new userns. Two strategies (WP-#114):
    //  • DEFAULT (single-uid): write our OWN single-id map "0 <outer> 1" + setgroups
    //    deny + gid_map — only container-root exists inside (byte-identical to the
    //    pre-#114 path).
    //  • RANGE (subid_sync = Some): a real non-root `--user` was requested AND the
    //    host has /etc/subuid + the newuidmap helpers. We canNOT write a range map
    //    ourselves (the kernel only allows the single self-map), so the OUTSIDE
    //    parent installs it via newuidmap/newgidmap. Signal "userns created", wait
    //    for the parent, then continue. We deliberately do NOT write setgroups=deny
    //    here — newgidmap writes gid_map WITH privilege, leaving setgroups usable so
    //    PID 1 can drop supplementary groups on the `--user` switch.
    match subid_sync {
        None => {
            if write_map("/proc/self/uid_map", &format!("0 {} 1\n", uid)).is_err()
                || write_map("/proc/self/setgroups", "deny\n").is_err()
                || write_map("/proc/self/gid_map", &format!("0 {} 1\n", gid)).is_err()
            {
                eprintln!("lightr-engine ns: uid/gid map failed");
                return 1;
            }
        }
        Some((ready_w, done_r)) => {
            let one = [1u8; 1];
            if unsafe { libc::write(ready_w, one.as_ptr() as *const libc::c_void, 1) } != 1 {
                eprintln!("lightr-engine ns: subid handshake (signal) failed");
                unsafe {
                    libc::close(ready_w);
                    libc::close(done_r);
                }
                signal_setup_failed(exec_ready_fd, "subid: handshake signal failed");
                return 1;
            }
            unsafe { libc::close(ready_w) };
            let mut status = [0u8; 1];
            let n = unsafe { libc::read(done_r, status.as_mut_ptr() as *mut libc::c_void, 1) };
            unsafe { libc::close(done_r) };
            if n != 1 || status[0] != 0 {
                eprintln!(
                    "lightr-engine ns: subid RANGE mapping failed (newuidmap/newgidmap) \
                     — cannot run as the requested non-root user"
                );
                signal_setup_failed(
                    exec_ready_fd,
                    "subid: range mapping failed (newuidmap/newgidmap)",
                );
                return 1;
            }
            // Maps installed by the parent; the captured `uid`/`gid` are unused here.
        }
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
        // process). The crux: the fresh procfs is mounted BEFORE pivot_root + the
        // put_old MNT_DETACH, while the host `/proc` inherited via CLONE_NEWNS is
        // still fully-visible — so the kernel
        // (`mount_too_revealing`/`fs_fully_visible`) permits a rootless fresh
        // procfs mount; and PID 1 is in the new pid ns, so that proc reflects the
        // new ns (`cat /proc/self/status` ⇒ `Pid: 1`). Post-pivot there would be
        // no fully-visible proc left → EPERM (the bug #96 fixes).
        // We are POST-fork here, so every failure MUST `libc::_exit(1)` (NOT
        // `return 1`, which was correct only in the setup process). The whole
        // rootfs+pivot phase — through the optional read-only remount — is done in
        // `setup_rootfs_and_pivot`, which fails closed (signals the pipe + `_exit`)
        // INSIDE on any error and returns the pre-compiled seccomp filter (the ONE
        // local that crosses back here for the LATE install below).
        let compiled_seccomp = setup_rootfs_and_pivot(
            rootfs,
            read_only,
            shm_size,
            tmpfs,
            bind_mounts,
            resolv_conf,
            hostname,
            add_host,
            seccomp,
            exec_ready_fd,
        );

        // chdir into the cwd-within-rootfs then execvp-style PATH-resolve argv[0]
        // against the CONTAINER rootfs (post-pivot). Runs in PID 1 BEFORE the
        // `--init` fork, so the resolved CString is copied across that fork.
        // Fail-closed INSIDE on an unresolvable argv[0] (signals the pipe + `_exit(127)`).
        let prog_resolved = chdir_and_resolve(cwd, command, env_path, exec_ready_fd);

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
                // ── great-grandchild: the workload (PID 2) — the shared apply+exec
                // tail (`apply_and_exec`) runs ulimits/oom EARLY, then caps →
                // apparmor → user → seccomp, arms the exec pipe, and execv's;
                // fail-closed via `_exit` throughout. Never returns.
                apply_and_exec(
                    desired_caps_vec.as_deref(),
                    apparmor,
                    user,
                    subid_sync.is_some(),
                    compiled_seccomp.as_ref(),
                    ulimits,
                    oom_score_adj,
                    exec_ready_fd,
                    &prog_resolved,
                    &argv_ptrs,
                );
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
            // No `--init`: the workload itself is PID 1. The shared apply+exec tail
            // (`apply_and_exec`) runs the same fixed order and execv's; never returns.
            apply_and_exec(
                desired_caps_vec.as_deref(),
                apparmor,
                user,
                subid_sync.is_some(),
                compiled_seccomp.as_ref(),
                ulimits,
                oom_score_adj,
                exec_ready_fd,
                &prog_resolved,
                &argv_ptrs,
            );
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
