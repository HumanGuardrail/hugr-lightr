//! WP-RC-RESTART setup helpers for the native supervisor — extracted from
//! `supervise_native.rs` (via `#[path]`) so the heart-loop file stays under the
//! 400-line godfile cap (FIX-#76 split). These are the per-concern, one-time /
//! per-spawn helpers the supervisor loop calls: spawn the child, start the port
//! forwarders, and `--rm` auto-clean on final exit. They carry no loop state.
//!
//! Declared as a child module of `supervise_native` via `#[path]`, so sibling
//! `run` items are reached through `crate::run::…` (an absolute path that is
//! robust to the extra nesting `#[path]` introduces).

use lightr_core::{LightrError, Result};

use crate::run::types::SpecOnDisk;

/// Spawn one child with the run's persisted command/env/identity in `run_cwd`,
/// writing its pid + a `running` status. Returns the spawned `Child` + its pid.
pub(super) fn spawn_child(
    dir: &std::path::Path,
    spec: &SpecOnDisk,
    run_cwd: &std::path::Path,
) -> Result<(std::process::Child, i32)> {
    // Append, not truncate, on a re-spawn so a restarting service's logs are not
    // lost. The first spawn creates the files; subsequent ones append.
    let stdout_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("stdout.log"))
        .map_err(LightrError::Io)?;
    let stderr_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("stderr.log"))
        .map_err(LightrError::Io)?;

    // WP-RUNFLAGS: `--entrypoint` prepends to the persisted command (Docker CMD).
    // `None` ⇒ argv == command (byte-identical to before).
    let argv = crate::run::bindmat::effective_argv(spec.entrypoint.as_deref(), &spec.command);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(run_cwd)
        // WP-DISC: explicit per-child env (compose service discovery + service
        // env). Empty for a plain `lightr run -d` (byte-identical to before).
        .envs(spec.env.iter().cloned())
        .stdout(std::process::Stdio::from(stdout_log))
        .stderr(std::process::Stdio::from(stderr_log));
    // WP-RC-USER: honor `-u`/`--user` (cfg(unix); None ⇒ current user).
    crate::run::spawn::apply_user(&mut cmd, spec.user.as_deref())?;
    // WP-HYG (#71): the child leads its own process group so `stop` reaps the
    // whole tree, not just the immediate child (see `spawn::set_own_process_group`).
    crate::run::spawn::set_own_process_group(&mut cmd);
    // RC-SEAM-FREEZE: per-field runtime-config appliers from the persisted spec
    // (all no-ops today — behaviour-preserving; a future RC WP fills one slot).
    crate::run::apply_cfg::apply_run_config_ondisk(spec, &mut cmd);
    // WP-RESLIMITS: apply the persisted resource caps to the detached child. On
    // Linux this installs the RLIMIT_AS/DATA pre_exec hook for `mem_limit_bytes`
    // (a hard cap — an over-cap child is killed). `cpu_limit_millis` has no
    // portable native cpu-share cap ⇒ honest Err (never silently enforced); a
    // memory cap off Linux is likewise an honest Err. Unlimited (both `None`) ⇒
    // no-op, so a run with no caps spawns byte-identically to before. Fail-closed:
    // an unenforceable cap stops the spawn rather than silently dropping it.
    let limits = lightr_core::ResourceLimits {
        memory_bytes: spec.mem_limit_bytes,
        cpu_millis: spec.cpu_limit_millis,
        // pids is cgroup-only; the native supervisor records the `pids_limit`
        // carry-field (recorded-only) but never enforces it here ⇒ never set.
        pids_max: None,
    };
    crate::limits::apply_native(&mut cmd, &limits)?;

    let child = cmd.spawn().map_err(LightrError::Io)?;
    let pid = child.id() as i32;
    std::fs::write(dir.join("pid"), format!("{pid}")).map_err(LightrError::Io)?;
    std::fs::write(dir.join("status"), "running").map_err(LightrError::Io)?;
    Ok((child, pid))
}

/// WP-RUNFLAGS: `--rm` — when the run's supervisor reaches its final exit, remove
/// the run dir + release its registry name (Docker `--rm` auto-clean). `rm=false`
/// ⇒ no-op (the dir persists, today's behaviour). The run `home` is the dir's
/// grandparent (`<home>/run/<id>`). Best-effort: a removal failure is swallowed —
/// the supervisor is exiting anyway and must not error on a cleanup race.
pub(super) fn maybe_auto_remove(dir: &std::path::Path, spec: &SpecOnDisk) {
    if !spec.rm {
        return;
    }
    // Release the name FIRST (so it frees even if the dir removal races).
    if let Some(name) = spec.name.as_deref() {
        if let Some(home) = dir.parent().and_then(|p| p.parent()) {
            let _ = crate::run::registry::release(home, name);
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Start the port forwarders for the run (held for the run's lifetime, across
/// re-spawns — the published port stays bound while the service restarts). A
/// bind failure is logged + skipped, never fatal. `_forwarders` is RETURNED (not
/// dropped) so the caller binds it for the supervisor's lifetime.
pub(super) fn start_forwarders(
    dir: &std::path::Path,
    spec: &SpecOnDisk,
) -> Vec<crate::portforward::Forwarder> {
    let mut forwarders: Vec<crate::portforward::Forwarder> = Vec::new();
    // WP-B2: bind each published port on its requested host interface. Prefer the
    // go-forward `ports2` channel (carries host_ip + proto); fall back to the
    // legacy `(host, container)` tuples (host_ip empty ⇒ `0.0.0.0`) for spec.json
    // written before `ports2` existed. The forward TARGET stays `127.0.0.1` (the
    // native run's server listens on loopback) — only the BIND interface changed.
    for (host_ip, host_port, container_port) in spec.published_ports() {
        let bind_ip = if host_ip.is_empty() {
            "0.0.0.0"
        } else {
            host_ip.as_str()
        };
        match crate::portforward::start_on(bind_ip, host_port, "127.0.0.1", container_port) {
            Ok(fwd) => forwarders.push(fwd),
            Err(e) => {
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("stderr.log"))
                {
                    let _ = writeln!(
                        f,
                        "lightr: publish {bind_ip}:{host_port} -> 127.0.0.1:{container_port} failed: {e}"
                    );
                }
            }
        }
    }
    forwarders
}
