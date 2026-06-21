//! WP-RC-RESTART — the native detached supervisor + its `--restart` re-spawn
//! loop (the heart). Moved out of `supervise.rs` so each file stays under the
//! 400-line godfile cap. The vz container path stays in `svz.rs`; `supervise`
//! dispatches here for the native host-process case.
//!
//! Shape: one-time setup (mount hydration, log files, port forwarders, health
//! config, the ctl endpoint) → an OUTER restart loop that spawns the child,
//! monitors it (serving ctl + probing health), and on exit consults the
//! `RestartPolicy`. `no` (the default) runs the child exactly once and exits —
//! byte-identical to the pre-WP-RC-RESTART supervisor. `always`/`unless-stopped`
//! re-spawn until an explicit stop; `on-failure[:max]` re-spawns on a nonzero
//! exit up to `max`. A small crash-loop backoff bounds a tight failure loop, and
//! an explicit `lightr stop`/`rm` (a stop marker, or a signal relayed through
//! ctl.sock) disables every restart.

use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::PathBuf;

use super::ctl::ctl_sock_path;
use super::memo::validate_mount_target;
use super::respawn;
use super::types::SpecOnDisk;
use crate::restart::RestartPolicy;

pub(super) fn supervise_native(
    dir: &std::path::Path,
    spec: &SpecOnDisk,
    store: &Store,
) -> Result<i32> {
    let cwd = PathBuf::from(&spec.cwd);

    // Hydrate mounts (same law as run_memoized), once for the run's lifetime.
    for m in &spec.mounts {
        validate_mount_target(&m.target)?;
        let dest = cwd.join(&m.target);
        lightr_index::hydrate(&dest, store, &m.ref_name)?;
    }

    // WP-RC-WORKDIR: honor `-w`/`--workdir` as the child's cwd (Docker WORKDIR),
    // creating it if absent. `None` ⇒ `cwd` unchanged + no mkdir.
    let run_cwd = super::spawn::resolve_workdir(&cwd, spec.workdir.as_deref())?;

    // WP-RC-RESTART: resolve the persisted policy. `None`/unparseable ⇒ `No`
    // (run once + exit, byte-identical to before).
    let policy = respawn::policy_from_spec(spec.restart.as_deref());

    // F-309 / WP-RC-4: load an optional healthcheck (probed on the monitor loop).
    let health_cfg = crate::healthcheck::load_for(dir)?;

    run_supervisor_loop(dir, spec, &cwd, &run_cwd, policy, health_cfg)
}

/// Spawn one child with the run's persisted command/env/identity in `run_cwd`,
/// writing its pid + a `running` status. Returns the spawned `Child` + its pid.
fn spawn_child(
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

    let mut cmd = std::process::Command::new(&spec.command[0]);
    cmd.args(&spec.command[1..])
        .current_dir(run_cwd)
        // WP-DISC: explicit per-child env (compose service discovery + service
        // env). Empty for a plain `lightr run -d` (byte-identical to before).
        .envs(spec.env.iter().cloned())
        .stdout(std::process::Stdio::from(stdout_log))
        .stderr(std::process::Stdio::from(stderr_log));
    // WP-RC-USER: honor `-u`/`--user` (cfg(unix); None ⇒ current user).
    super::spawn::apply_user(&mut cmd, spec.user.as_deref())?;
    // RC-SEAM-FREEZE: per-field runtime-config appliers from the persisted spec
    // (all no-ops today — behaviour-preserving; a future RC WP fills one slot).
    super::apply_cfg::apply_run_config_ondisk(spec, &mut cmd);
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
    };
    crate::limits::apply_native(&mut cmd, &limits)?;

    let child = cmd.spawn().map_err(LightrError::Io)?;
    let pid = child.id() as i32;
    std::fs::write(dir.join("pid"), format!("{pid}")).map_err(LightrError::Io)?;
    std::fs::write(dir.join("status"), "running").map_err(LightrError::Io)?;
    Ok((child, pid))
}

/// Start the port forwarders for the run (held for the run's lifetime, across
/// re-spawns — the published port stays bound while the service restarts). A
/// bind failure is logged + skipped, never fatal. `_forwarders` is RETURNED (not
/// dropped) so the caller binds it for the supervisor's lifetime.
fn start_forwarders(
    dir: &std::path::Path,
    spec: &SpecOnDisk,
) -> Vec<crate::portforward::Forwarder> {
    let mut forwarders: Vec<crate::portforward::Forwarder> = Vec::new();
    for &(host_port, container_port) in &spec.ports {
        match crate::portforward::start(host_port, container_port) {
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
                        "lightr: publish 127.0.0.1:{host_port} -> 127.0.0.1:{container_port} failed: {e}"
                    );
                }
            }
        }
    }
    forwarders
}

#[cfg(unix)]
fn run_supervisor_loop(
    dir: &std::path::Path,
    spec: &SpecOnDisk,
    cwd: &std::path::Path,
    run_cwd: &std::path::Path,
    policy: RestartPolicy,
    health_cfg: Option<crate::healthcheck::Healthcheck>,
) -> Result<i32> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};

    // Forwarders + ctl endpoint live for the whole run (across re-spawns).
    let _forwarders = start_forwarders(dir, spec);
    let sock_path = ctl_sock_path(dir);
    let listener = UnixListener::bind(&sock_path).map_err(LightrError::Io)?;
    listener.set_nonblocking(true).map_err(LightrError::Io)?;

    // Health state machine + monotonic launch instant (started once; `ps` shows
    // "starting" before the first probe round).
    let health_launched = Instant::now();
    let mut health_state = crate::healthcheck::HealthState::default();
    if health_cfg.is_some() {
        crate::healthcheck::write_state(dir, health_state.status);
    }
    let mut next_probe = Instant::now();

    // `stopped` latches an EXPLICIT stop (a `signal` op relayed through ctl.sock,
    // or the lifecycle/stop stop marker) so no policy re-spawns after it.
    let mut stopped = false;
    let mut restarts_done: u32 = 0;

    let final_exit = 'restart: loop {
        let (mut child, child_pid) = spawn_child(dir, spec, run_cwd)?;

        // Per-child monitor: serve ctl.sock + poll child + probe health.
        let exit_code = loop {
            if let Some(ref hc) = health_cfg {
                if Instant::now() >= next_probe {
                    let passed = crate::healthcheck::probe_once(hc, cwd);
                    let in_start = health_launched.elapsed().as_secs() < hc.start_period_s;
                    health_state.record(passed, in_start, hc.retries);
                    crate::healthcheck::write_state(dir, health_state.status);
                    next_probe = Instant::now() + Duration::from_secs(hc.interval_s.max(1));
                }
            }

            if let Some(status) = child.try_wait().map_err(LightrError::Io)? {
                break status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
            }

            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(1))).ok();
                    stream.set_write_timeout(Some(Duration::from_secs(1))).ok();
                    let mut reader = BufReader::new(&stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() {
                        let line = line.trim();
                        if let Ok(req) = serde_json::from_str::<serde_json::Value>(line) {
                            let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
                            let reply: serde_json::Value = match op {
                                "status" => serde_json::json!({"status": "running"}),
                                "signal" => {
                                    if let Some(sig) = req.get("sig").and_then(|v| v.as_i64()) {
                                        // WP-RC-RESTART: a signal relayed through
                                        // ctl.sock is the `stop`/`kill` path — an
                                        // EXPLICIT stop. Latch it (+ persist the
                                        // marker) so no policy re-spawns the child.
                                        stopped = true;
                                        respawn::write_stop_marker(dir);
                                        unsafe {
                                            libc::kill(child_pid, sig as libc::c_int);
                                        }
                                        serde_json::json!({"ok": true})
                                    } else {
                                        serde_json::json!({"ok": false})
                                    }
                                }
                                _ => serde_json::json!({"error": "unknown op"}),
                            };
                            let mut reply_bytes = serde_json::to_vec(&reply).unwrap_or_default();
                            reply_bytes.push(b'\n');
                            let mut w = &stream;
                            let _ = w.write_all(&reply_bytes);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {}
            }

            std::thread::sleep(Duration::from_millis(100));
        };

        // Also honor a stop marker written by the lifecycle/stop DIRECT-kill path
        // (no ctl round-trip), so `rm -f` / a kill-without-ctl disables restart.
        if respawn::stop_requested(dir) {
            stopped = true;
        }

        if !respawn::should_restart(policy, exit_code, restarts_done, stopped) {
            break 'restart exit_code;
        }

        // Re-spawn: bump the count, back off (bounds a crash-loop), reflect the
        // restart in the status file so `ps`/watchers see movement.
        restarts_done = restarts_done.saturating_add(1);
        std::fs::write(dir.join("status"), format!("restarting {restarts_done}"))
            .map_err(LightrError::Io)?;
        std::thread::sleep(respawn::backoff_for(restarts_done));
    };

    std::fs::write(dir.join("status"), format!("exited {final_exit}")).map_err(LightrError::Io)?;
    let _ = std::fs::remove_file(&sock_path);
    Ok(final_exit)
}

#[cfg(windows)] // WIN-PATH: named-pipe control server, identical JSON wire protocol.
fn run_supervisor_loop(
    dir: &std::path::Path,
    spec: &SpecOnDisk,
    cwd: &std::path::Path,
    run_cwd: &std::path::Path,
    policy: RestartPolicy,
    health_cfg: Option<crate::healthcheck::Healthcheck>,
) -> Result<i32> {
    use super::ctl::ctl_pipe_name;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let _forwarders = start_forwarders(dir, spec);

    let sentinel = ctl_sock_path(dir);
    let pipe_name = ctl_pipe_name(dir);

    let health_launched = Instant::now();
    let mut health_state = crate::healthcheck::HealthState::default();
    if health_cfg.is_some() {
        crate::healthcheck::write_state(dir, health_state.status);
    }
    let mut next_probe = Instant::now();

    let mut restarts_done: u32 = 0;

    let final_exit = 'restart: loop {
        let (mut child, child_pid) = spawn_child(dir, spec, run_cwd)?;

        // Per-child named-pipe control server (one server per child, like the
        // unix ctl.sock is served per-iteration here). A `signal` op writes the
        // stop marker (win path), which the post-exit check reads.
        let done = Arc::new(AtomicBool::new(false));
        let done_srv = Arc::clone(&done);
        let server_exited = Arc::new(AtomicBool::new(false));
        let server_exited_srv = Arc::clone(&server_exited);
        let pipe_name_srv = pipe_name.clone();
        let dir_srv = dir.to_path_buf();
        let server = std::thread::spawn(move || {
            win::win_pipe_server_loop(&pipe_name_srv, child_pid, &dir_srv, &done_srv);
            server_exited_srv.store(true, Ordering::SeqCst);
        });
        std::fs::write(&sentinel, b"live").map_err(LightrError::Io)?;

        let exit_code = loop {
            if let Some(ref hc) = health_cfg {
                if Instant::now() >= next_probe {
                    let passed = crate::healthcheck::probe_once(hc, cwd);
                    let in_start = health_launched.elapsed().as_secs() < hc.start_period_s;
                    health_state.record(passed, in_start, hc.retries);
                    crate::healthcheck::write_state(dir, health_state.status);
                    next_probe = Instant::now() + Duration::from_secs(hc.interval_s.max(1));
                }
            }
            if let Some(status) = child.try_wait().map_err(LightrError::Io)? {
                break status.code().unwrap_or(1);
            }
            std::thread::sleep(Duration::from_millis(100));
        };

        // Tear down this child's pipe server before deciding on a re-spawn.
        done.store(true, Ordering::SeqCst);
        while !server_exited.load(Ordering::SeqCst) {
            win::win_pipe_nudge(&pipe_name);
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = server.join();

        let stopped = respawn::stop_requested(dir);
        if !respawn::should_restart(policy, exit_code, restarts_done, stopped) {
            break 'restart exit_code;
        }

        restarts_done = restarts_done.saturating_add(1);
        std::fs::write(dir.join("status"), format!("restarting {restarts_done}"))
            .map_err(LightrError::Io)?;
        std::thread::sleep(respawn::backoff_for(restarts_done));
    };

    std::fs::write(dir.join("status"), format!("exited {final_exit}")).map_err(LightrError::Io)?;
    let _ = std::fs::remove_file(&sentinel);
    Ok(final_exit)
}

// WIN-PATH: the Windows named-pipe control-server helpers live in
// `supervise_win.rs`, pulled in via `#[path]`. `#[cfg(windows)]`, so the unix CI
// gate never compiles it.
#[cfg(windows)]
#[path = "supervise_win.rs"]
mod win;

#[cfg(test)]
#[path = "supervise_native_tests.rs"]
mod tests;
