//! ns_impl::engine — the `NsEngine` type and its `Engine::run` implementation:
//! the shim-process orchestration (spec capture, the subid RANGE plan, the setup
//! fork, the outside-parent subid dance, and the final waitpid). The container
//! launch core proper lives in `run.rs` (`run_in_namespaces`). All items live
//! inside the Linux-gated `ns_impl` module (see `ns/mod.rs`).

use super::run::run_in_namespaces;
use super::signal::wait_to_exit_code;
use super::subid_ns::{plan_subid_range, run_parent_subid_dance, wants_subid_range, SubidSetup};
use crate::engine::spec::{BindMount, ExecSpec, TmpfsMount, Ulimit};
use crate::engine::Engine;
use lightr_core::{LightrError, Result};

pub struct NsEngine;

impl Engine for NsEngine {
    fn run(&self, spec: &ExecSpec) -> Result<i32> {
        let rootfs = spec
            .rootfs
            .ok_or_else(|| LightrError::InvalidRef("ns engine requires a rootfs".to_string()))?;

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
        // `--oom-score-adj` (Docker parity): the OOM killer score adjustment.
        // Captured (by Copy) pre-fork like `ulimits` so PID 1 owns its copy;
        // applied EARLY in PID 1 (alongside `apply_ulimits_if_any`) via a write
        // to /proc/self/oom_score_adj. `None` ⇒ no-op (byte-identical to the
        // pre-feature path).
        let oom_score_adj: Option<i32> = spec.oom_score_adj;

        // WP-#114: real non-root `--user` on the rootless ns engine. The default
        // single-uid map ("0 <outer> 1") makes ONLY container-root exist inside, so
        // a non-root `--user` cannot be honored (#113 fails it closed). To run a
        // workload as a real non-root in-container uid we need a subordinate-id
        // RANGE map — which an unprivileged process canNOT write itself; it must be
        // written from OUTSIDE the userns by the setuid-root newuidmap/newgidmap
        // helpers (authorized against /etc/subuid + /etc/subgid). THIS process (the
        // shim) is that outside parent: it forks the setup child below, then maps it.
        //
        // We enter the RANGE path ONLY when (a) a NON-root `--user` was requested AND
        // (b) the host actually has a subuid/subgid allocation + the helpers. If
        // either is missing, `subid_setup` is None ⇒ the byte-identical single-uid
        // path runs and the non-root `--user` hits the #113 honest-error (no silent
        // root). Two one-byte pipes synchronize the dance: the child signals "userns
        // created" on `ready`, the parent installs the maps and replies on `done`.
        let subid_setup: Option<SubidSetup> = if wants_subid_range(user.as_deref()) {
            plan_subid_range().and_then(|plan| {
                let mut ready = [0 as libc::c_int; 2];
                let mut done = [0 as libc::c_int; 2];
                if unsafe { libc::pipe(ready.as_mut_ptr()) } != 0 {
                    return None;
                }
                if unsafe { libc::pipe(done.as_mut_ptr()) } != 0 {
                    unsafe {
                        libc::close(ready[0]);
                        libc::close(ready[1]);
                    }
                    return None;
                }
                Some(SubidSetup {
                    plan,
                    ready_r: ready[0],
                    ready_w: ready[1],
                    done_r: done[0],
                    done_w: done[1],
                })
            })
        } else {
            None
        };
        // The CHILD-side ends (write `ready`, read `done`) passed into the setup
        // process; `Some` ALSO tells PID 1 the target uid IS mapped (real drop).
        let subid_sync: Option<(libc::c_int, libc::c_int)> =
            subid_setup.as_ref().map(|s| (s.ready_w, s.done_r));

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
                // WP-#114: this setup child only WRITES `ready` + READS `done`;
                // close the parent's ends so a parent crash can't wedge it.
                if let Some(s) = subid_setup.as_ref() {
                    unsafe {
                        libc::close(s.ready_r);
                        libc::close(s.done_w);
                    }
                }
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
                    oom_score_adj,
                    subid_sync,
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
                // WP-#114: the newuidmap/newgidmap RANGE dance. The setup child has
                // unshared its userns; as the OUTSIDE parent we install a subuid
                // RANGE map on it via the setuid-root helpers, then release it. Runs
                // BEFORE waitpid (which blocks for the container's life). Closes the
                // child's pipe ends first (we only read `ready` + write `done`); the
                // dance closes its own ends. On failure it still replies on `done`
                // (status byte) so the child never wedges — the child then fails
                // closed (signals the exec pipe + exits), so the backend sees an
                // error, never a false `Running`.
                if let Some(s) = subid_setup.as_ref() {
                    unsafe {
                        libc::close(s.ready_w);
                        libc::close(s.done_r);
                    }
                    run_parent_subid_dance(child_pid, &s.plan, s.ready_r, s.done_w);
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
