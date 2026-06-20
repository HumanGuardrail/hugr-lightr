//! Tests for spawn/supervise/ps/stop/logs/exec_in lifecycle.
#![cfg(test)]

use crate::run::exec::exec_in;
use crate::run::paths::{read_status_file, write_spec_json};
use crate::run::ps::ps;
use crate::run::spawn::spawn_detached;
use crate::run::stop::stop;
use crate::run::supervise::supervise;
use crate::run::types::{RunSpec, SpecOnDisk};
use crate::{healthcheck, run::ctl::ctl_sock_path};
use lightr_store::Store;
use std::fs;

fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = super::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("LIGHTR_HOME", home.path());
    (home, guard)
}

fn make_store(dir: &std::path::Path) -> Store {
    Store::open(dir.join("store")).expect("store open")
}

// -----------------------------------------------------------------------
// Helper: create a run dir + spec.json and launch supervise() in a thread.
// Returns (home_path, run_dir, thread_handle).
// Unit tests cannot use spawn_detached (requires real `lightr` binary via
// current_exe) so we call supervise() directly in a thread instead.
// -----------------------------------------------------------------------
fn start_supervised(
    home_path: &std::path::Path,
    cwd: &std::path::Path,
    command: Vec<String>,
) -> (std::path::PathBuf, std::thread::JoinHandle<i32>) {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let id = format!("{nanos}-test");
    let run_dir = home_path.join("run").join(&id);
    fs::create_dir_all(&run_dir).unwrap();

    let spec_on_disk = SpecOnDisk {
        cwd: cwd.to_string_lossy().into_owned(),
        command,
        env_keys: vec![],
        mounts: vec![],
        detached: false,
        created_at_unix: nanos / 1_000_000_000,
        ports: vec![],
        env_explicit: vec![],
        engine: "native".to_string(),
        rootfs_ref: None,
        env: vec![],
        ..Default::default()
    };
    write_spec_json(&run_dir, &spec_on_disk).unwrap();

    let run_dir_clone = run_dir.clone();
    let t = std::thread::spawn(move || supervise(&run_dir_clone).unwrap_or(-1));
    (run_dir, t)
}

// -----------------------------------------------------------------------
// detach_lifecycle: supervisor sleep 5 → ps shows running → stop → ps exited
// (uses supervise() directly in a thread — spawn_detached needs real binary)
// -----------------------------------------------------------------------
#[test]
fn detach_lifecycle() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();

    let (run_dir, _supervisor_thread) =
        start_supervised(&home_path, cwd, vec!["sleep".to_string(), "10".to_string()]);

    // Give supervisor time to write pid+status+ctl.sock
    let startup_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if ctl_sock_path(&run_dir).exists()
            && read_status_file(&run_dir)
                .map(|s| s == "running")
                .unwrap_or(false)
        {
            break;
        }
        if std::time::Instant::now() >= startup_deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // ps should show it running
    let infos = ps(&home_path).expect("ps");
    let id = run_dir.file_name().unwrap().to_string_lossy().into_owned();
    let found = infos.iter().find(|i| i.id == id);
    assert!(found.is_some(), "run not found in ps output");
    let info = found.unwrap();
    assert!(info.running, "run should be running");

    // stop it (grace=2s)
    let exit_code = stop(&run_dir, 2).expect("stop");
    // exit code after SIGTERM/SIGKILL: 143, 137, or 0 (if supervisor exited first)
    assert!(
        exit_code == 143 || exit_code == 137 || exit_code == 0,
        "unexpected exit code: {exit_code}"
    );

    // ps should now show not running
    let infos2 = ps(&home_path).expect("ps2");
    let found2 = infos2.iter().find(|i| i.id == id);
    // Either not found (dir removed) or found as not-running
    if let Some(info2) = found2 {
        assert!(!info2.running, "run should not be running after stop");
    }
}

// -----------------------------------------------------------------------
// logs_non_follow: write known content via supervisor, check log files
// (uses supervise() directly in a thread — spawn_detached needs real binary)
// -----------------------------------------------------------------------
#[test]
fn logs_non_follow() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();

    let (run_dir, supervisor_thread) = start_supervised(
        &home_path,
        cwd,
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo STDOUT_CONTENT; echo STDERR_CONTENT >&2".to_string(),
        ],
    );

    // Wait for supervisor to finish (process is short-lived)
    let _ = supervisor_thread.join();

    // Check stdout.log content
    let stdout_content = fs::read_to_string(run_dir.join("stdout.log")).unwrap_or_default();
    assert!(
        stdout_content.contains("STDOUT_CONTENT"),
        "stdout.log missing STDOUT_CONTENT: {stdout_content:?}"
    );

    // Check stderr.log content
    let stderr_content = fs::read_to_string(run_dir.join("stderr.log")).unwrap_or_default();
    assert!(
        stderr_content.contains("STDERR_CONTENT"),
        "stderr.log missing STDERR_CONTENT: {stderr_content:?}"
    );
}

// -----------------------------------------------------------------------
// exec_in_cwd: exec_in should run in the spec's cwd
// -----------------------------------------------------------------------
#[test]
fn exec_in_cwd_correctness() {
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().to_path_buf();
    let store = make_store(&home_path);

    let spec = RunSpec {
        cwd: cwd.clone(),
        inputs: vec![],
        command: vec!["sleep".to_string(), "30".to_string()],
        env_keys: vec![],
        mounts: vec![],
        secrets: vec![],
        configs: vec![],
        ports: vec![],
        env_explicit: vec![],
        workdir: None,
        user: None,
        restart: None,
        stop_signal: None,
        ..Default::default()
    };

    let handle = spawn_detached(&spec, &store).expect("spawn_detached");
    let run_dir = handle.dir.clone();

    // Give supervisor time to write spec.json
    std::thread::sleep(std::time::Duration::from_millis(300));

    // exec_in with pwd — should print the run's cwd
    // We capture by using a temp file
    let out_file = tmp.path().join("pwd_output.txt");
    let cmd = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("pwd > {}", out_file.display()),
    ];

    let exit_code = exec_in(&run_dir, &cmd).expect("exec_in");
    assert_eq!(exit_code, 0, "exec_in should exit 0");

    let output = fs::read_to_string(&out_file).unwrap_or_default();
    let canonical_cwd = cwd.canonicalize().unwrap();
    assert!(
        output.trim() == canonical_cwd.to_string_lossy().as_ref(),
        "exec_in cwd mismatch: got {output:?}, expected {:?}",
        canonical_cwd
    );

    // Clean up: stop the sleeper
    let _ = stop(&run_dir, 1);
}

// -----------------------------------------------------------------------
// supervisor_health_flips_unhealthy: a FAILING healthcheck writes
// "unhealthy" to <run_dir>/health and ps surfaces it.
// -----------------------------------------------------------------------
#[test]
fn supervisor_health_flips_unhealthy() {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();

    // Build a run dir with a long-lived child + a persisted FAILING probe.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let id = format!("{nanos}-health");
    let run_dir = home_path.join("run").join(&id);
    fs::create_dir_all(&run_dir).unwrap();

    let spec_on_disk = SpecOnDisk {
        cwd: cwd.to_string_lossy().into_owned(),
        command: vec!["sleep".to_string(), "10".to_string()],
        env_keys: vec![],
        mounts: vec![],
        detached: false,
        created_at_unix: nanos / 1_000_000_000,
        ports: vec![],
        env_explicit: vec![],
        engine: "native".to_string(),
        rootfs_ref: None,
        env: vec![],
        ..Default::default()
    };
    write_spec_json(&run_dir, &spec_on_disk).unwrap();
    healthcheck::save_for(
        &run_dir,
        &healthcheck::Healthcheck {
            cmd: "exit 1".to_string(), // always fails ⇒ Unhealthy
            interval_s: 1,
            timeout_s: 0,
            start_period_s: 0,
            retries: 0,
        },
    )
    .unwrap();

    let run_dir_clone = run_dir.clone();
    let t = std::thread::spawn(move || supervise(&run_dir_clone).unwrap_or(-1));

    // Poll until the verdict FLIPS to Unhealthy. The first write is `Starting`;
    // a fixed wait that breaks on the first verdict races the watchdog on slow
    // CI runners (caught `Starting` before the probe flipped it). Poll for the
    // target state with a generous deadline instead.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut health = None;
    while Instant::now() < deadline {
        health = healthcheck::read_state(&run_dir);
        if health == Some(healthcheck::Health::Unhealthy) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        health,
        Some(healthcheck::Health::Unhealthy),
        "a failing healthcheck must flip the run to Unhealthy"
    );

    // ps surfaces the same verdict while the run is alive.
    let infos = ps(&home_path).expect("ps");
    let info = infos.iter().find(|i| i.id == id).expect("run in ps");
    assert_eq!(info.health, Some(healthcheck::Health::Unhealthy));

    // Clean up the sleeper + supervisor.
    let _ = stop(&run_dir, 2);
    let _ = t.join();
}

// -----------------------------------------------------------------------
// ps_enrich_fields: ps() surfaces engine, ports, and rootfs_ref from
// SpecOnDisk (WP-PS-ENRICH). Verifies defaults (native / empty / None)
// and explicit values without spinning up a real supervisor.
// -----------------------------------------------------------------------
#[test]
fn ps_enrich_fields() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let (home, _env_guard) = isolated_home();
    let home_path = home.path().to_path_buf();

    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    // --- Case A: native run, no ports, no rootfs_ref ---
    let id_a = format!("{nanos}-enrich-a");
    let run_dir_a = home_path.join("run").join(&id_a);
    fs::create_dir_all(&run_dir_a).unwrap();
    let spec_a = SpecOnDisk {
        cwd: cwd.to_string_lossy().into_owned(),
        command: vec!["true".to_string()],
        env_keys: vec![],
        mounts: vec![],
        detached: true,
        created_at_unix: nanos / 1_000_000_000,
        ports: vec![],
        env_explicit: vec![],
        engine: "native".to_string(),
        rootfs_ref: None,
        env: vec![],
        ..Default::default()
    };
    write_spec_json(&run_dir_a, &spec_a).unwrap();
    // Write exited status so ps picks it up without a real supervisor.
    fs::write(run_dir_a.join("status"), "exited 0").unwrap();

    // --- Case B: vz run, one port pair, with rootfs_ref ---
    let id_b = format!("{nanos}-enrich-b");
    let run_dir_b = home_path.join("run").join(&id_b);
    fs::create_dir_all(&run_dir_b).unwrap();
    let spec_b = SpecOnDisk {
        cwd: cwd.to_string_lossy().into_owned(),
        command: vec!["/bin/nginx".to_string()],
        env_keys: vec![],
        mounts: vec![],
        detached: true,
        created_at_unix: nanos / 1_000_000_000,
        ports: vec![(8080, 80)],
        env_explicit: vec![],
        engine: "vz".to_string(),
        rootfs_ref: Some("my-rootfs".to_string()),
        env: vec![],
        ..Default::default()
    };
    write_spec_json(&run_dir_b, &spec_b).unwrap();
    fs::write(run_dir_b.join("status"), "exited 0").unwrap();

    let infos = ps(&home_path).expect("ps");

    let info_a = infos.iter().find(|i| i.id == id_a).expect("run A in ps");
    assert_eq!(info_a.engine, "native", "case A: engine must be native");
    assert!(info_a.ports.is_empty(), "case A: ports must be empty");
    assert_eq!(info_a.rootfs_ref, None, "case A: rootfs_ref must be None");

    let info_b = infos.iter().find(|i| i.id == id_b).expect("run B in ps");
    assert_eq!(info_b.engine, "vz", "case B: engine must be vz");
    assert_eq!(
        info_b.ports,
        vec![(8080u16, 80u16)],
        "case B: ports must match"
    );
    assert_eq!(
        info_b.rootfs_ref,
        Some("my-rootfs".to_string()),
        "case B: rootfs_ref must match"
    );
}
