//! `lightr run` engine/flag policy guards + input resolvers, split out of
//! `mod.rs`/`helpers.rs` to keep both under the 400-LOC godfile cap. Every guard
//! here returns `Option<i32>`/`Result<_, i32>` (the exit code) — the `return` stays
//! in `run()`, so control flow is byte-identical to the pre-split inline blocks.

use lightr_core::ResourceLimits;
use lightr_engine::EngineKind;
use lightr_run::{Mount, PortMap, RunSpec, StoreFile};
use lightr_store::Store;

use super::runflags::RunFlags;
use super::RcConfig;

/// WP-#92: `--privileged` cannot be honestly enforced on the rootless `ns` engine
/// (no real privilege in an unprivileged user namespace), so it is HONEST-ERRORED
/// (exit 2) BEFORE any provisioning rather than silently recorded — a silent no-op
/// on a security flag gives false security. `Some(2)` to reject, `None` to allow.
pub(super) fn rc_privileged_policy(rc: &RcConfig) -> Option<i32> {
    if rc.privileged {
        eprintln!(
            "lightr: --privileged is not supported on the rootless ns engine (no real \
             privilege in an unprivileged user namespace); tracked for --engine vz"
        );
        return Some(2);
    }
    None
}

/// WP-RUNFLAGS: `--name` + `--rm` need a run dir/id, which only the DETACHED path
/// creates (a foreground run is stateless — just the Action Cache). So they are
/// detached-only; using them without `-d` is an honest exit 2, never a silent no-op.
pub(super) fn detached_only_flags_policy(runflags: &RunFlags, detach: bool) -> Option<i32> {
    if runflags.name.is_some() && !detach {
        eprintln!("lightr: --name requires -d (a named run is a detached container)");
        return Some(2);
    }
    if runflags.rm && !detach {
        eprintln!("lightr: --rm requires -d (a foreground run leaves no run dir to remove)");
        return Some(2);
    }
    None
}

/// WP-#94/#106/#108: `--cap-add`/`--cap-drop`, `--apparmor`, and `--seccomp` do
/// REAL enforcement only on the `ns` engine. For any OTHER engine they are
/// HONEST-ERRORED (exit 2) BEFORE provisioning rather than silently recorded —
/// native is no sandbox by design, and vz caps/LSM/seccomp live inside the guest
/// (not managed by this shim). A silent no-op on a security flag would give false
/// security (the exact failure WP-#92 refused). `Some(2)` to reject, `None` to allow.
pub(super) fn engine_capability_policy(engine: EngineKind, rc: &RcConfig) -> Option<i32> {
    if engine != EngineKind::Ns && (!rc.cap_add.is_empty() || !rc.cap_drop.is_empty()) {
        eprintln!(
            "lightr: --cap-add/--cap-drop capability enforcement is implemented only on \
             the rootless ns engine (--engine ns); native is no sandbox and vz caps live \
             inside the guest — refusing to run rather than give false security"
        );
        return Some(2);
    }
    if engine != EngineKind::Ns && rc.apparmor.is_some() {
        eprintln!(
            "lightr: --apparmor (AppArmor LSM enforcement) is implemented only on the \
             rootless ns engine (--engine ns); native is no sandbox and the vz LSM lives \
             inside the guest — refusing to run rather than give false security"
        );
        return Some(2);
    }
    if engine != EngineKind::Ns && rc.seccomp.is_some() {
        eprintln!(
            "lightr: --seccomp (seccomp-bpf enforcement) is implemented only on the \
             rootless ns engine (--engine ns); native is no sandbox and vz seccomp lives \
             inside the guest — refusing to run rather than give false security"
        );
        return Some(2);
    }
    None
}

/// `--tmpfs`/`--ulimit` are handled on `ns` (real mount / setrlimit) and, for
/// `--ulimit`, also `native` (pre_exec setrlimit). Only `vz` has NO handling (both
/// would live inside the guest, not managed by this shim), so honest-error
/// `vz`+`--tmpfs`/`--ulimit` (exit 2) BEFORE provisioning rather than silently drop
/// them. `Some(2)` to reject, `None` to allow.
pub(super) fn vz_mount_policy(engine: EngineKind, runflags: &RunFlags) -> Option<i32> {
    if engine == EngineKind::Vz && !runflags.tmpfs.is_empty() {
        eprintln!(
            "lightr: --tmpfs is not supported on the vz engine (tmpfs mounts live \
             inside the guest, not managed by this shim); use --engine ns for a real \
             tmpfs mount or --engine native for a scratch directory"
        );
        return Some(2);
    }
    if engine == EngineKind::Vz && !runflags.ulimit.is_empty() {
        eprintln!(
            "lightr: --ulimit is not supported on the vz engine (process resource \
             limits live inside the guest, not managed by this shim); use --engine ns \
             or --engine native"
        );
        return Some(2);
    }
    None
}

/// WP-#95: `--init` is ENFORCED on the `ns` engine (real PID-1 reaper). On any OTHER
/// engine it is a recorded-only carry-slot (native is a host process with no pid
/// namespace; vz reaps via its own guest PID 1) — say so honestly rather than imply
/// a reaper that won't run. Side-effect only (an honest note), never a return code.
pub(super) fn init_engine_note(init: bool, engine: EngineKind) {
    if init && engine != EngineKind::Ns {
        eprintln!(
            "lightr: note: --init runs a real PID-1 reaper only on the rootless ns \
             engine (--engine ns); here it is recorded only (no pid namespace to reap in)"
        );
    }
}

/// WP-#90: a pids cap needs cgroup v2 `pids.max`, which only the `ns` engine owns.
/// vz is a microVM (no delegated per-container cgroup via the shim) — so a
/// `--pids-limit --engine vz` request is honest-errored HERE, before the VM boots,
/// rather than silently dropped. `Some(2)` to reject, `None` to allow.
pub(super) fn vz_pids_policy(engine: EngineKind, limits: &ResourceLimits) -> Option<i32> {
    if engine == EngineKind::Vz && limits.pids_max.is_some() {
        eprintln!(
            "lightr: vz engine cannot enforce a pids limit (no per-container cgroup \
             in the microVM); use --engine ns"
        );
        return Some(2);
    }
    None
}

/// WP-RC-4: a non-detached `--health-cmd` is fail-loud supervisor-only (the
/// healthcheck watchdog is owned by the supervisor, which only the detached path
/// spawns) — never silently dropped. Side-effect only (an honest note).
pub(super) fn healthcheck_detach_note(has_healthcheck: bool, detach: bool) {
    if has_healthcheck && !detach {
        eprintln!(
            "lightr: --health-cmd is wired for detached runs only (-d); the \
             healthcheck watchdog is owned by the supervisor — running without it"
        );
    }
}

/// Networking Phase 1 policy for `-p/--publish` (frozen, honest — enforced in this
/// order). A published service is a long-running server ⇒ it must be detached; and
/// publishing is wired only for the native detached path + the vz detached
/// container path (`--engine vz --rootfs <img>`). Other engines + vz-without-rootfs
/// are Phase 2 — an honest error, never a dropped port. `Some(2)` to reject.
pub(super) fn publish_policy(
    publish_raw: &[String],
    detach: bool,
    engine: EngineKind,
    rootfs_ref: Option<&str>,
) -> Option<i32> {
    if !publish_raw.is_empty() {
        // 1. A published service is a long-running server ⇒ it must be detached.
        if !detach {
            eprintln!("lightr: -p/--publish requires -d (a published service runs detached)");
            return Some(2);
        }
        // 2. Publishing is wired for the native detached path + the vz detached
        //    container path (WP-NET2: `--engine vz --rootfs <img>`); other engines
        //    + vz-without-rootfs are Phase 2 — an honest error, never a dropped port.
        let native = engine == EngineKind::Native && rootfs_ref.is_none();
        let vz_container = engine == EngineKind::Vz && rootfs_ref.is_some();
        if !native && !vz_container {
            eprintln!(
                "lightr: -p/--publish is wired for the native and `--engine vz --rootfs` \
                 detached paths; other engines are Phase 2"
            );
            return Some(2);
        }
    }
    None
}

/// `--add-host HOST:IP` ⇒ `(hostname, ip)` pairs for the ns engine's /etc/hosts
/// write. Already value-validated as `HOST:IP` in `RawRunFlags::resolve` (a
/// malformed entry was an exit 2 there), so the split is infallible here; a stray
/// bad entry is defensively skipped.
pub(super) fn resolve_add_host_pairs(runflags: &RunFlags) -> Vec<(String, String)> {
    runflags
        .add_host
        .iter()
        .filter_map(|raw| {
            raw.split_once(':')
                .map(|(h, ip)| (h.to_string(), ip.to_string()))
        })
        .collect()
}

/// Parse secrets/configs (F-309) — split `NAME=REF` for each raw entry. `kind` is
/// `"secret"` or `"config"` (used in the error message). Fail-closed: a malformed
/// entry is an honest `Err(exit_code)` (the caller keeps the `return`).
pub(super) fn resolve_store_files(raws: &[String], kind: &str) -> Result<Vec<StoreFile>, i32> {
    let mut out: Vec<StoreFile> = Vec::new();
    for raw in raws {
        out.push(super::parse_store_file(raw, kind)?);
    }
    Ok(out)
}

/// Parse `--mount REF:TARGET` specs into [`Mount`]s. Fail-closed: a malformed spec
/// is an honest `Err(exit_code)` (the caller keeps the `return`).
pub(super) fn resolve_mounts(raws: &[String]) -> Result<Vec<Mount>, i32> {
    let mut out: Vec<Mount> = Vec::new();
    for raw in raws {
        out.push(super::parse_mount(raw)?);
    }
    Ok(out)
}

/// WP-B2: build the published-port set for the vz detached container path. Consumes
/// the range-aware, host-ip-carrying `-p` parser (`8000-8002:8000-8002` ⇒ 3 maps;
/// `127.0.0.1:H:C` ⇒ loopback), then — when `publish_all` — auto-publishes every
/// TCP port the rootfs image EXPOSEs, de-duplicated against explicit `-p` host
/// ports. Fail-closed: a malformed `-p` spec is an honest `Err(exit_code)`.
pub(super) fn resolve_detached_ports(
    publish_raw: &[String],
    publish_all: bool,
    rootfs_ref: &str,
    store: &Store,
) -> Result<Vec<PortMap>, i32> {
    let mut ports: Vec<PortMap> = Vec::new();
    for raw in publish_raw {
        let mut maps = super::flags::publish::parse_publish_spec(raw)?;
        ports.append(&mut maps);
    }
    if publish_all {
        for pm in super::helpers::expose_port_maps(rootfs_ref, store) {
            if !ports.iter().any(|p| p.host == pm.host) {
                ports.push(pm);
            }
        }
    }
    Ok(ports)
}

/// WP-NET2: build the [`RunSpec`] for the vz detached container path. A pure value
/// builder — folds the already-parsed inputs (mounts/secrets/configs/ports/env) and
/// the resolved run-config (`rc`) + run-flags (`runflags`) carry-fields into the
/// spec persisted to spec.json. Byte-identical to the pre-split inline literal.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_detached_spec(
    cwd: std::path::PathBuf,
    command: &[String],
    env_keys: &[String],
    mounts: Vec<Mount>,
    secrets: Vec<StoreFile>,
    configs: Vec<StoreFile>,
    ports: Vec<PortMap>,
    env_explicit: Vec<(String, String)>,
    workdir: Option<&str>,
    user: Option<&str>,
    restart: Option<&str>,
    stop_signal: Option<&str>,
    limits: ResourceLimits,
    rc: &RcConfig,
    runflags: &RunFlags,
) -> RunSpec {
    RunSpec {
        cwd,
        inputs: Vec::new(),
        command: command.to_vec(),
        env_keys: env_keys.to_vec(),
        mounts,
        secrets,
        configs,
        ports,
        env_explicit,
        // RUNTIME flags persisted to spec.json; the native supervisor honors them.
        workdir: workdir.map(String::from),
        user: user.map(String::from),
        restart: restart.map(String::from),
        stop_signal: stop_signal.map(String::from),
        // WP-RESLIMITS: carry the parsed `--memory`/`--cpus` to spec.json so
        // the vz supervisor reads them back (the VM applies a hard mem/vcpu
        // cap — `vz_caps`). RUNTIME-ONLY, never keyed.
        limits,
        // WP-RC-FLAGS: the 11 run-config carry-fields (RUNTIME-ONLY, never
        // keyed). Persisted to spec.json + honored by the apply seam where the
        // native engine can; honest per-field note otherwise (see apply_cfg).
        hostname: rc.hostname.clone(),
        labels: rc.labels.clone(),
        cap_add: rc.cap_add.clone(),
        cap_drop: rc.cap_drop.clone(),
        privileged: rc.privileged,
        tty: rc.tty,
        init: rc.init,
        read_only: rc.read_only,
        oom_score_adj: rc.oom_score_adj,
        pids_limit: rc.pids_limit,
        shm_size: rc.shm_size,
        // WP-RUNFLAGS: `--name`/`--rm`/`--entrypoint` + `-v`/`--tmpfs` carry-
        // fields. Persisted to spec.json; honored on the native supervisor
        // path. RUNTIME-ONLY (never keyed). All-default ⇒ no-op.
        volumes: runflags.volumes.clone(),
        tmpfs: runflags.tmpfs.clone(),
        entrypoint: runflags.entrypoint.clone(),
        name: runflags.name.clone(),
        rm: runflags.rm,
        // WP-NET3: the vz container-networking carry-fields, off the C9 seam.
        // RUNTIME-ONLY, never keyed. `--network Some(..)` ⇒ the vz supervisor
        // (svz) create-or-opens the per-network registry, joins it, and
        // attaches the shared cross-process L2 switch (mesh NIC eth1). All
        // empty/None ⇒ the single-NAT-NIC path, byte-identical to before.
        network: runflags.network.clone(),
        network_alias: runflags.network_alias.clone(),
        add_host: runflags.add_host.clone(),
        dns: runflags.dns.clone(),
    }
}
