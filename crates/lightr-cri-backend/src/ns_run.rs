//! WP-#99 (CRI slice 1): the `ns`-run descriptor — the ONE serialization
//! contract between the CRI backend (which builds it) and the `__ns-run` shim in
//! `lightr-cri-serve` (which consumes it, builds an `ExecSpec`, and drives the
//! `ns` engine). It lives HERE, in `lightr-cri-backend`, because `lightr-cri-serve`
//! already depends on this crate — so both sides share a SINGLE type and can
//! never drift in field order/spelling (a copy-paste of the struct on each side
//! is exactly the silent-drift bug this avoids).
//!
//! Transport: JSON in the env var `LIGHTR_NSRUN_DESC` (the backend sets it on the
//! shim child; the shim reads + `serde_json::from_str`s it, then `remove_var`s it
//! BEFORE running the engine so it never leaks into the container). This mirrors
//! `__ns-exec`'s `LIGHTR_NSEXEC_DESC` and — crucially — leaves the shim child's
//! STDIN FREE so an attachable container (`ContainerConfig.stdin == true`, e.g.
//! the critest attach test's interactive `/bin/sh`) inherits a live stdin the
//! backend can write to via `open_attach`. The descriptor never touches disk and
//! carries no secrets.
//!
//! HISTORY: slice 1 (#99) transported the descriptor on the shim's STDIN. But the
//! ns engine `execv`s with the shim's INHERITED stdio (it does not redirect fds
//! 0/1/2), so a stdin-descriptor left the workload's fd 0 at EOF — a bare
//! interactive `/bin/sh` (the cri-tools "should support attach" container, started
//! with `Stdin: true, StdinOnce: true`) read EOF and exited BEFORE attach could
//! connect → `container ... is not Running (state=Exited)`. Moving the descriptor
//! off stdin (the proven `__ns-exec` pattern) frees fd 0 for the workload/attach.

use serde::{Deserialize, Serialize};

/// Everything the `__ns-run` shim needs to run ONE container under the `ns`
/// engine, joined into the pod's existing netns. Mirrors the `ExecSpec` fields
/// the shim sets; runtime-only values (never a memo key — there is no memo here).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDescriptor {
    /// Materialized image rootfs to pivot into (the persistent per-container
    /// hydrate dir). Becomes `ExecSpec.rootfs`.
    pub rootfs: std::path::PathBuf,
    /// The full argv (program + args), already including the `tail -f /dev/null`
    /// keep-alive fallback when the container declared no command. argv[0] is the
    /// program. Becomes `ExecSpec.command`.
    pub argv: Vec<String>,
    /// Working directory inside the container; empty ⇒ `/`. Becomes `ExecSpec.cwd`
    /// (the ns engine chdirs to it within the pivoted rootfs).
    pub cwd: String,
    /// Environment as (key, value) pairs. Becomes `ExecSpec.env`.
    pub env: Vec<(String, String)>,
    /// Path of the pod's pinned netns (e.g. `/run/netns/<id>`). The ns engine
    /// `setns(CLONE_NEWNET)`s into it BEFORE the userns unshare. Becomes
    /// `ExecSpec.join_netns`. `None` would fall back to host networking — the
    /// backend only takes the ns path when this is `Some`.
    pub netns_path: Option<String>,
    /// The explicit cgroup-v2 leaf name (`lightr-cri-<cid>`, a flat leaf — dash
    /// not slash, so `stop` rebuilds the same path). Becomes
    /// `ExecSpec.cgroup_name`; the backend's `stop` writes its `cgroup.kill`.
    pub cgroup_name: String,
    /// `--read-only`: remount the rootfs RO. Becomes `ExecSpec.read_only`.
    pub read_only: bool,
    /// `--shm-size` in bytes (None ⇒ default 64 MiB). Becomes `ExecSpec.shm_size`.
    pub shm_size: Option<u64>,
    /// `--init`: run a minimal PID-1 reaper. Becomes `ExecSpec.init`.
    pub init: bool,
    /// Capabilities to ADD (CRI/Docker style, no `CAP_` prefix needed). Becomes
    /// `ExecSpec.cap_add`.
    pub cap_add: Vec<String>,
    /// Capabilities to DROP. Becomes `ExecSpec.cap_drop`.
    pub cap_drop: Vec<String>,
    /// WP-#102: the raw fd NUMBER of the exec-readiness pipe's WRITE end. The backend
    /// creates the pipe and spawns this shim WITHOUT `O_CLOEXEC` on the write end, so
    /// the fd is INHERITED across the re-exec and is open at the SAME number in the
    /// shim — only its number travels here (over JSON), not the fd itself. Becomes
    /// `ExecSpec.exec_ready_fd`; the ns engine sets it CLOEXEC right before the
    /// container's `execv` so a successful exec auto-closes it (the backend's reader
    /// sees EOF ⇒ persist Running). `None` ⇒ no readiness signalling (host path).
    #[serde(default)]
    pub exec_ready_fd: Option<i32>,
    /// WP-#106 (KPI 4): the AppArmor profile NAME to exec the container under, mapped
    /// from `rec.config.security.apparmor` in `build_ns_plan` (CRI `Localhost` ⇒ the
    /// loaded profile name; `Unconfined` ⇒ `"unconfined"`; `RuntimeDefault` ⇒ `None`,
    /// i.e. inherit, for now). Becomes `ExecSpec.apparmor`; the ns engine applies it
    /// via aa_change_onexec right before the container's `execv` (fail-closed). `None`
    /// ⇒ no profile change (today's behavior — security is usually None until the
    /// cross-repo seam #89 maps the kubelet profile through). `#[serde(default)]` keeps
    /// old descriptors deserializing as `None`.
    #[serde(default)]
    pub apparmor: Option<String>,
    /// WP-#108 (seccomp): the PATH to an OCI seccomp JSON profile to enforce on the
    /// container (or "unconfined"), mapped from `rec.config.security.seccomp` in
    /// `build_ns_plan` (CRI `Localhost` ⇒ the profile path `localhost_ref`;
    /// `Unconfined` ⇒ `"unconfined"`; `RuntimeDefault` ⇒ `None`, i.e. inherit, for
    /// now). Becomes `ExecSpec.seccomp`; the ns engine compiles it before pivot and
    /// installs the cBPF filter right before the container's `execv` (fail-closed).
    /// `None` ⇒ no profile change (today's behavior — `rec.config.security` is usually
    /// None until the cross-repo seam maps the kubelet profile through).
    /// `#[serde(default)]` keeps old descriptors deserializing as `None`.
    #[serde(default)]
    pub seccomp: Option<String>,
    /// WP-#107 (CRI GAP 1, "starting container with volume"): the CRI
    /// `ContainerConfig.mounts`, host-side already realpath'd in `build_ns_plan` (the
    /// symlink-host-path spec resolves `host_path` BEFORE it reaches here — a host
    /// concern). Becomes `ExecSpec.bind_mounts`; the ns engine `mkdir -p`s each target
    /// under the rootfs and bind-mounts the host source onto it (RO when `readonly`),
    /// fail-closed. `#[serde(default)]` ⇒ old descriptors deserialize as empty.
    #[serde(default)]
    pub mounts: Vec<NsBindMount>,
    /// WP-#107 (CRI GAP 2, "DNS config"): the full `/etc/resolv.conf` CONTENT,
    /// synthesized from the sandbox `DnsConfig` in `build_ns_plan` (nameserver/search/
    /// options lines). Becomes `ExecSpec.resolv_conf`; the ns engine writes it into
    /// `<rootfs>/etc/resolv.conf` before pivot. `None` ⇒ no DNS config (image untouched).
    #[serde(default)]
    pub resolv_conf: Option<String>,
    /// WP-#107 (CRI GAP 3, "set hostname"): the sandbox hostname. Becomes
    /// `ExecSpec.hostname`; the ns engine unshares a UTS ns, `sethostname`s it, and
    /// writes `<rootfs>/etc/hostname`. `None`/empty ⇒ no UTS ns (unchanged behavior).
    #[serde(default)]
    pub hostname: Option<String>,
}

/// WP-#107 (CRI GAP 1): one CRI volume mount carried on the descriptor — mirrors
/// `lightr_engine::BindMount` (the engine type can't derive serde without pulling
/// serde into `lightr-engine`, so we carry this serde twin and map at the shim, the
/// SAME pattern the descriptor uses for every other field). `host_path` is the
/// already-realpath'd HOST source; `container_path` the in-container destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NsBindMount {
    pub host_path: String,
    pub container_path: String,
    pub readonly: bool,
}

/// Env var carrying the [`RunDescriptor`] JSON to the `__ns-run` shim. Mirrors
/// `__ns-exec`'s `LIGHTR_NSEXEC_DESC`. Off-stdin so the workload's stdin (fd 0) is
/// FREE for attach (see the module doc). Removed by the shim before `engine.run`
/// so it does not leak into the container (the ns engine `execv`s with inherited
/// env).
pub const NSRUN_DESC_ENV: &str = "LIGHTR_NSRUN_DESC";

/// Entry point for the `__ns-run` re-exec shim: read a [`RunDescriptor`] (JSON)
/// from the `LIGHTR_NSRUN_DESC` env var, build an `ExecSpec`, run it under the
/// `ns` engine joined into the pod's netns, and `exit` with the workload's code.
/// NEVER returns.
///
/// This lives in `lightr-cri-backend` (not in `lightr-cri-serve`) so the shim
/// reuses THIS crate's `lightr-engine` + `serde_json` deps — `lightr-cri-serve`
/// is its own isolated workspace and only forwards `__ns-run` here. The ns engine
/// inherits this process's stdio (so the container's stdin/stdout/stderr are the
/// fds the backend wired: stdin = the attach pipe when `config.stdin`, else
/// /dev/null; stdout/stderr = the CRI-log tee pipes), and blocks until the
/// workload exits. Fail-closed: any setup error exits non-zero.
pub fn run_shim() -> ! {
    // Descriptor from the env var (JSON), then REMOVE it immediately so the ns
    // engine's `execv` (which inherits this process's env) never carries it into
    // the container. The shim is single-threaded here, so remove_var is sound.
    let json = match std::env::var(NSRUN_DESC_ENV) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lightr-cri ns-run: {NSRUN_DESC_ENV} unset: {e}");
            std::process::exit(1);
        }
    };
    std::env::remove_var(NSRUN_DESC_ENV);
    let desc: RunDescriptor = match serde_json::from_str(&json) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lightr-cri ns-run: bad descriptor JSON: {e}");
            std::process::exit(1);
        }
    };

    let engine = match lightr_engine::engine_for(lightr_engine::EngineKind::Ns) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("lightr-cri ns-run: ns engine unavailable: {e}");
            std::process::exit(1);
        }
    };

    // Borrows held alive for the ExecSpec.
    let cwd = std::path::PathBuf::from(&desc.cwd);
    let netns_path = desc.netns_path.as_ref().map(std::path::PathBuf::from);
    // WP-#107 (CRI GAP 1): map the serde-twin mounts to the engine's BindMount type
    // (held alive for the ExecSpec borrow, like `cwd`/`netns_path`).
    let bind_mounts: Vec<lightr_engine::BindMount> = desc
        .mounts
        .iter()
        .map(|m| lightr_engine::BindMount {
            host_path: m.host_path.clone(),
            container_path: m.container_path.clone(),
            readonly: m.readonly,
        })
        .collect();

    let spec = lightr_engine::ExecSpec {
        cwd: &cwd,
        command: &desc.argv,
        rootfs: Some(desc.rootfs.as_path()),
        limits: Default::default(),
        net: false,
        net_isolate: false,
        net_fd: None,
        net_mac: None,
        mounts: &[],
        env: &desc.env,
        workdir: None,
        user: None,
        // WP-#107 (CRI GAP 3): the sandbox hostname — drives a UTS unshare +
        // sethostname + /etc/hostname in the ns engine. `None` ⇒ unchanged.
        hostname: desc.hostname.as_deref(),
        add_host: &[],
        dns: &[],
        mesh_ip: None,
        read_only: desc.read_only,
        shm_size: desc.shm_size,
        cap_drop: &desc.cap_drop,
        cap_add: &desc.cap_add,
        init: desc.init,
        // WP-#99: the crux — join the pod netns, pin the killable cgroup leaf.
        join_netns: netns_path.as_deref(),
        cgroup_name: Some(desc.cgroup_name.as_str()),
        // WP-#102: the inherited write end of the backend's exec-readiness pipe; the
        // ns engine threads it to PID 1 (CLOEXEC-before-execv). `None` on the host path.
        exec_ready_fd: desc.exec_ready_fd,
        // WP-#106: the AppArmor profile mapped from the CRI security context (ready for
        // the cross-repo seam #89; `None` today). The ns engine applies it via
        // aa_change_onexec right before the container's execv (fail-closed).
        apparmor: desc.apparmor.as_deref(),
        // WP-#108: the seccomp profile path mapped from the CRI security context
        // (`None` today until the seam maps it). The ns engine compiles it before
        // pivot and installs the cBPF filter right before execv (fail-closed).
        seccomp: desc.seccomp.as_deref(),
        // WP-#107 (CRI GAP 1): the CRI volume bind mounts (host_path already
        // realpath'd in build_ns_plan). The ns engine binds each into the rootfs
        // before pivot, fail-closed. Empty ⇒ unchanged.
        bind_mounts: &bind_mounts,
        // WP-#107 (CRI GAP 2): the synthesized /etc/resolv.conf content. The ns
        // engine writes it into the rootfs before pivot. `None` ⇒ image untouched.
        resolv_conf: desc.resolv_conf.as_deref(),
        // `--tmpfs` is a Docker-run flag; the CRI path has no tmpfs source today.
        tmpfs: &[],
    };

    match engine.run(&spec) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("lightr-cri ns-run: engine.run failed: {e}");
            std::process::exit(1);
        }
    }
}
