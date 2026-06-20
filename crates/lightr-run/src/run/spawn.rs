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
        // WP-RC-1 (R-KEY): persist the KEYED user `-e`/`--env-file` env to
        // spec.json so a restart re-applies it (distinct from the UNKEYED
        // discovery `env` above). Empty for runs with no `-e`/`--env-file`.
        env_explicit: spec.env_explicit.clone(),
        // WP-RC-WORKDIR: persist `-w`/`--workdir` so the detached supervisor
        // honors it as the native child's cwd (`supervise` reads it back).
        // `None` for runs with no `-w` ⇒ child runs in `cwd`, as before. RUNTIME
        // ONLY — never a memo-key input (detached runs aren't memoized anyway).
        workdir: spec.workdir.clone(),
        // WP-RC-USER: persist `-u`/`--user` so the detached supervisor honors it
        // as the native child's POSIX identity (`supervise` reads it back, sets
        // uid/gid before exec). `None` for runs with no `-u` ⇒ child runs as the
        // current user, as before. RUNTIME ONLY — never a memo-key input.
        user: spec.user.clone(),
        // R-SPECDISK freeze-gate fields: defaults until the Wave-A/B WPs
        // populate them (no behaviour change here).
        ..Default::default()
    };
    write_spec_json(&dir, &spec_on_disk)?;

    launch_supervisor(&dir)?;

    Ok(RunHandle { id, dir })
}

/// Re-launch the detached supervisor (`__supervise <dir>`) for a run dir that
/// already holds a valid `spec.json`. Extracted from `spawn_detached_engine` so
/// the lifecycle primitive `respawn_run` re-spawns a stopped run in its SAME
/// dir/id without duplicating the detach (setsid / DETACHED_PROCESS) logic.
/// Behaviour for the spawn path is byte-identical to the inline code it replaced.
pub(super) fn launch_supervisor(dir: &std::path::Path) -> Result<()> {
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
    Ok(())
}

/// WP-RC-WORKDIR: resolve the directory the run's process must execute in, and
/// CREATE it if absent (Docker creates `WORKDIR`). `workdir = None` ⇒ `base`
/// unchanged, with NO mkdir — so a run with no `-w` is byte-identical to before
/// (the base cwd is the caller's existing, already-present dir). `Some(w)` ⇒
/// `base.join(w)` (a relative `w` nests; an absolute `w` replaces), created
/// recursively. Both the synchronous native path (`memo`) and the detached
/// supervisor (`supervise`) call this so `-w` is honored on every native run.
pub(super) fn resolve_workdir(
    base: &std::path::Path,
    workdir: Option<&str>,
) -> Result<std::path::PathBuf> {
    match workdir {
        None => Ok(base.to_path_buf()),
        Some(w) => {
            let dir = base.join(w);
            std::fs::create_dir_all(&dir).map_err(LightrError::Io)?;
            Ok(dir)
        }
    }
}

/// WP-RC-USER: honor `-u`/`--user` on a native child `Command` before exec.
///
/// `user = None` ⇒ NO-OP (the child runs as the current user, byte-identical to
/// before — behavior-preserving). `Some(spec)` is parsed as `uid[:gid]`
/// (numeric — the faithful path) or `name[:group]` (best-effort numeric: a
/// purely-numeric component is taken as an id; a NON-numeric name can't be
/// resolved without the container's `/etc/passwd`, so it is an HONEST error
/// rather than a silent fallback).
///
/// On unix, a parsed uid/gid is applied via `CommandExt::uid`/`gid`. Setting a
/// uid/gid different from the current process's requires root; lightr's native
/// runtime is NOT a root daemon (unlike Docker's), so the actual privilege check
/// is deferred to the kernel at `spawn`/exec, which fails with `EPERM` — an
/// HONEST error surfaced to the caller (never silently ignored).
///
/// On windows, a POSIX uid/gid has no meaning: `Some(_)` is an HONEST error and
/// `None` is the unchanged no-op. The parsed `cmd`/`uid`/`gid` bindings are
/// consumed under both cfgs (no unused-variable warning on the windows gate).
pub(super) fn apply_user(cmd: &mut std::process::Command, user: Option<&str>) -> Result<()> {
    let spec = match user {
        None => return Ok(()),
        Some(s) => s,
    };

    // Parse `id[:group]`. An empty spec is rejected (honest, not a silent no-op).
    if spec.is_empty() {
        return Err(LightrError::InvalidRef(
            "invalid --user value: empty".to_string(),
        ));
    }
    let (uid_part, gid_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };

    let parse_id = |part: &str, which: &str| -> Result<u32> {
        part.parse::<u32>().map_err(|_| {
            LightrError::InvalidRef(format!(
                "invalid --user {which} {part:?}: name resolution needs the container's \
                 /etc/passwd — use a numeric uid[:gid] (the faithful native path)"
            ))
        })
    };

    let uid = parse_id(uid_part, "uid")?;
    let gid = match gid_part {
        Some(g) => Some(parse_id(g, "gid")?),
        None => None,
    };

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.uid(uid);
        if let Some(gid) = gid {
            cmd.gid(gid);
        }
        // The EPERM (setting a different uid as non-root) surfaces at spawn/exec
        // as an honest io::Error — lightr native is not a root daemon.
        Ok(())
    }

    #[cfg(not(unix))]
    {
        // POSIX uid/gid has no meaning on windows. Consume the bindings so the
        // windows clippy gate sees no unused variables, and fail HONESTLY.
        let _ = (cmd, uid, gid);
        Err(LightrError::InvalidRef(
            "--user (POSIX uid/gid) is not supported on this host".to_string(),
        ))
    }
}
