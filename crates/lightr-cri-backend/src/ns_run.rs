//! WP-#99 (CRI slice 1): the `ns`-run descriptor — the ONE serialization
//! contract between the CRI backend (which builds it) and the `__ns-run` shim in
//! `lightr-cri-serve` (which consumes it, builds an `ExecSpec`, and drives the
//! `ns` engine). It lives HERE, in `lightr-cri-backend`, because `lightr-cri-serve`
//! already depends on this crate — so both sides share a SINGLE type and can
//! never drift in field order/spelling (a copy-paste of the struct on each side
//! is exactly the silent-drift bug this avoids).
//!
//! Transport: JSON over the shim child's STDIN (the backend writes it; the shim
//! reads stdin to EOF and `serde_json::from_slice`s it). No temp file, no env —
//! the descriptor never touches disk, and stdin is private to the parent/child.

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
}

/// Entry point for the `__ns-run` re-exec shim: read a [`RunDescriptor`] (JSON)
/// from STDIN to EOF, build an `ExecSpec`, run it under the `ns` engine joined
/// into the pod's netns, and `exit` with the workload's code. NEVER returns.
///
/// This lives in `lightr-cri-backend` (not in `lightr-cri-serve`) so the shim
/// reuses THIS crate's `lightr-engine` + `serde_json` deps — `lightr-cri-serve`
/// is its own isolated workspace and only forwards `__ns-run` here. The ns engine
/// inherits this process's stdio (so the container's stdout/stderr flow to the
/// pipes the backend wired into the CRI log tee), and blocks until the workload
/// exits. Fail-closed: any setup error exits non-zero.
pub fn run_shim() -> ! {
    use std::io::Read;

    let mut buf = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
        eprintln!("lightr-cri ns-run: read descriptor from stdin failed: {e}");
        std::process::exit(1);
    }
    let desc: RunDescriptor = match serde_json::from_slice(&buf) {
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
        hostname: None,
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
    };

    match engine.run(&spec) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("lightr-cri ns-run: engine.run failed: {e}");
            std::process::exit(1);
        }
    }
}
