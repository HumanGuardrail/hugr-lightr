//! Detached process spawning: spawn_detached, spawn_detached_with_health,
//! spawn_detached_engine.

use lightr_core::{LightrError, Result};
use lightr_engine::EngineKind;
use lightr_store::Store;

use super::paths::{new_run_id, run_dir_for_id, write_spec_json};
use super::types::{MountOnDisk, RunHandle, RunSpec, SpecOnDisk};

pub fn spawn_detached(spec: &RunSpec, store: &Store) -> Result<RunHandle> {
    spawn_detached_engine(spec, store, None, EngineKind::Native, None, &[])
}

/// `spawn_detached` plus an optional healthcheck (F-309). When `hc` is
/// `Some`, it is persisted into the run dir (`healthcheck.json`) and the
/// detached supervisor probes it on its interval, writing `Healthy`/`Unhealthy`
/// to `<run_dir>/health` so `ps` can surface liveness. The healthcheck is a
/// post-result probe and is **not** part of the memo key (build-spec-parity.md
/// §0); it never affects caching or the run's output.
///
/// `spawn_detached` delegates here with `None`, so its 2 existing callers (the
/// CLI run handler and compose's `start_service_detached`) keep their behaviour
/// unchanged.
pub fn spawn_detached_with_health(
    spec: &RunSpec,
    store: &Store,
    hc: Option<&crate::healthcheck::Healthcheck>,
) -> Result<RunHandle> {
    spawn_detached_engine(spec, store, hc, EngineKind::Native, None, &[])
}

/// `spawn_detached_with_health` plus the engine + rootfs ref (WP-NET2). The
/// `native` path (`engine = Native`, `rootfs_ref = None`) is the existing
/// supervisor: it spawns the command as a host process. The `vz` path
/// (`engine = Vz` + a `rootfs_ref`) boots a Linux container in a microVM inside
/// the supervisor and forwards each published port to the guest's DHCP IP — the
/// `-p`-for-a-Linux-image case. The engine + rootfs ref are persisted to
/// spec.json (serde-defaulted, so old native runs read back unchanged) and are
/// NOT memo-key inputs (a detached run is never memoized).
///
/// WP-DISC: `env` is an explicit set of `(key, value)` pairs applied to the
/// detached NATIVE child (compose service discovery: `<PEER>_HOST`/`<PEER>_PORT`
/// plus the service's own env). It is persisted to spec.json (serde-defaulted)
/// and is NOT a memo-key input — runtime addressing, like ports, and detached
/// runs aren't memoized anyway. The vz branch ignores it.
pub fn spawn_detached_engine(
    spec: &RunSpec,
    _store: &Store,
    hc: Option<&crate::healthcheck::Healthcheck>,
    engine: EngineKind,
    rootfs_ref: Option<&str>,
    env: &[(String, String)],
) -> Result<RunHandle> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let id = new_run_id();
    let dir = run_dir_for_id(&id);
    std::fs::create_dir_all(&dir).map_err(LightrError::Io)?;

    // Persist the healthcheck (if any) BEFORE forking the supervisor, so the
    // supervisor finds it on startup. Not in the memo key (§0).
    if let Some(hc) = hc {
        crate::healthcheck::save_for(&dir, hc)?;
    }

    let created_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let spec_on_disk = SpecOnDisk {
        cwd: spec.cwd.to_string_lossy().into_owned(),
        command: spec.command.clone(),
        env_keys: spec.env_keys.clone(),
        mounts: spec
            .mounts
            .iter()
            .map(|m| MountOnDisk {
                ref_name: m.ref_name.clone(),
                target: m.target.clone(),
            })
            .collect(),
        detached: true,
        created_at_unix,
        ports: spec.ports.iter().map(|p| (p.host, p.container)).collect(),
        engine: engine.as_str().to_string(),
        rootfs_ref: rootfs_ref.map(|s| s.to_string()),
        env: env.to_vec(),
    };
    write_spec_json(&dir, &spec_on_disk)?;

    let exe = std::env::current_exe().map_err(LightrError::Io)?;
    let dir_str = dir.to_string_lossy().into_owned();

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["__supervise", &dir_str]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    // WIN-PATH: Windows has no `setsid`/process-session model. The closest
    // correctness analog is detaching the supervisor from the parent's console
    // and giving it its own process group so a Ctrl-C to the launcher does not
    // tear down the detached supervisor. Full process-tree containment via job
    // objects is a future ring. Validatable only on a real Windows box.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
        };
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_NO_WINDOW);
    }

    cmd.spawn().map_err(LightrError::Io)?;

    Ok(RunHandle { id, dir })
}
