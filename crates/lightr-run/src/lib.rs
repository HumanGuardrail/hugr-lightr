//! lightr-run — frozen contract: build-spec v2 §6 + build-spec-r1 §2.
//! Memo key, native exec, replay, supervisor, ps, logs, stop, exec_in.

use lightr_core::{Digest, LightrError, Result, OUTPUT_CAP_BYTES};
use lightr_index::{scan, Index};
use lightr_store::Store;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub struct RunSpec {
    pub cwd: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub command: Vec<String>,
    pub env_keys: Vec<String>,
    // R1: mounts hydrated CoW into <cwd>/<target> pre-key/pre-exec
    // (build-spec-r1 §2); part of the memo key in order.
    pub mounts: Vec<Mount>,
}

pub struct Mount {
    pub ref_name: String,
    pub target: String,
}

pub struct RunOutcome {
    pub key: Digest,
    pub hit: bool,
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

// ---------------------------------------------------------------------------
// AC record format "LRR1":
//   4B magic b"LRR1"
//   4B i32  exit_code  (LE)
//  32B      stdout digest
//  32B      stderr digest
// Total: 72 bytes
// ---------------------------------------------------------------------------

const AC_MAGIC: &[u8; 4] = b"LRR1";
const AC_RECORD_LEN: usize = 4 + 4 + 32 + 32; // 72

fn encode_ac_record(exit_code: i32, stdout_d: &Digest, stderr_d: &Digest) -> Vec<u8> {
    let mut buf = Vec::with_capacity(AC_RECORD_LEN);
    buf.extend_from_slice(AC_MAGIC);
    buf.extend_from_slice(&exit_code.to_le_bytes());
    buf.extend_from_slice(&stdout_d.0);
    buf.extend_from_slice(&stderr_d.0);
    buf
}

fn decode_ac_record(bytes: &[u8]) -> Option<(i32, Digest, Digest)> {
    if bytes.len() != AC_RECORD_LEN {
        return None;
    }
    if &bytes[..4] != AC_MAGIC {
        return None;
    }
    let exit_code = i32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let mut stdout_raw = [0u8; 32];
    let mut stderr_raw = [0u8; 32];
    stdout_raw.copy_from_slice(&bytes[8..40]);
    stderr_raw.copy_from_slice(&bytes[40..72]);
    Some((exit_code, Digest(stdout_raw), Digest(stderr_raw)))
}

// ---------------------------------------------------------------------------
// Mount target validation
// ---------------------------------------------------------------------------

fn validate_mount_target(t: &str) -> Result<()> {
    use std::path::Path;
    let p = Path::new(t);
    // Must be relative
    if p.is_absolute() {
        return Err(LightrError::InvalidRef(format!(
            "mount target escapes cwd: {t}"
        )));
    }
    // Must not contain ".." components
    for component in p.components() {
        if component == std::path::Component::ParentDir {
            return Err(LightrError::InvalidRef(format!(
                "mount target escapes cwd: {t}"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Key assembly — exact order per contract:
//   update(b"lightr/run/v1\0")
//   for each input (spec.inputs; if empty use [spec.cwd]) in GIVEN order:
//       canonicalize against cwd
//       scan(path, &mut Index::load_for(path)?)
//       update(rel-path-as-given bytes + b"\0" + manifest.digest().0)
//   for each arg in spec.command:
//       update((arg.len() as u64).to_le_bytes() + arg bytes)
//   env: collect spec.env_keys sorted, for each present in std::env:
//       update(key + b"=" + value + b"\0")
//       absent keys: update(key + b"\x01")
//   update(std::env::consts::OS + "-" + std::env::consts::ARCH)
//   mounts: for each mount in order:
//       validate target (reject "..")
//       update(ref_name bytes + [0x02] + mount-ref's CURRENT root digest bytes)
//   key = finalize
// ---------------------------------------------------------------------------

/// Shared private key-assembly fn. `hydrate_mounts` controls whether to
/// actually hydrate into cwd (true for run_memoized, false for predict).
fn assemble_key(spec: &RunSpec, store: &Store, hydrate_mounts: bool) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();

    // Domain separator
    hasher.update(b"lightr/run/v1\0");

    // Input manifests
    let inputs: Vec<&PathBuf> = if spec.inputs.is_empty() {
        vec![&spec.cwd]
    } else {
        spec.inputs.iter().collect()
    };

    for input_path in inputs {
        // Canonicalize against cwd
        let abs_path = if input_path.is_absolute() {
            input_path.clone()
        } else {
            spec.cwd.join(input_path)
        };
        let canonical = abs_path.canonicalize().map_err(LightrError::Io)?;

        // Scan to get the manifest
        let mut index = Index::load_for(&canonical)?;
        let report = scan(&canonical, &mut index)?;

        // Use rel-path-as-given bytes
        let rel_path_bytes = input_path.as_os_str().as_encoded_bytes();
        hasher.update(rel_path_bytes);
        hasher.update(b"\0");
        hasher.update(&report.manifest.digest().0);
    }

    // Command args
    for arg in &spec.command {
        let len = arg.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(arg.as_bytes());
    }

    // Env keys — sorted
    let mut sorted_keys = spec.env_keys.clone();
    sorted_keys.sort();
    for key in &sorted_keys {
        if let Some(val) = std::env::var_os(key) {
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(val.as_encoded_bytes());
            hasher.update(b"\0");
        } else {
            // Absent key: contribute key + \x01
            hasher.update(key.as_bytes());
            hasher.update(b"\x01");
        }
    }

    // Target triple: OS-ARCH
    let triple = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    hasher.update(triple.as_bytes());

    // Mounts key contribution
    for mount in &spec.mounts {
        validate_mount_target(&mount.target)?;

        if hydrate_mounts {
            // Hydrate the mount into cwd/target
            let dest = spec.cwd.join(&mount.target);
            lightr_index::hydrate(&dest, store, &mount.ref_name)?;
        }

        // Key contribution: ref_name bytes + [0x02] + mount root digest
        let rec = store
            .ref_get(&mount.ref_name)?
            .ok_or_else(|| LightrError::RefNotFound(mount.ref_name.clone()))?;
        hasher.update(mount.ref_name.as_bytes());
        hasher.update(&[0x02u8]);
        hasher.update(&rec.root.0);
    }

    Ok(Digest(*hasher.finalize().as_bytes()))
}

// Keep the old build_key for backward-compat within existing tests
fn build_key(spec: &RunSpec) -> Result<Digest> {
    // No store needed for no-mounts case; but we must handle it.
    // For the unmounted path (used by existing tests), we short-circuit.
    let mut hasher = blake3::Hasher::new();

    hasher.update(b"lightr/run/v1\0");

    let inputs: Vec<&PathBuf> = if spec.inputs.is_empty() {
        vec![&spec.cwd]
    } else {
        spec.inputs.iter().collect()
    };

    for input_path in inputs {
        let abs_path = if input_path.is_absolute() {
            input_path.clone()
        } else {
            spec.cwd.join(input_path)
        };
        let canonical = abs_path.canonicalize().map_err(LightrError::Io)?;
        let mut index = Index::load_for(&canonical)?;
        let report = scan(&canonical, &mut index)?;
        let rel_path_bytes = input_path.as_os_str().as_encoded_bytes();
        hasher.update(rel_path_bytes);
        hasher.update(b"\0");
        hasher.update(&report.manifest.digest().0);
    }

    for arg in &spec.command {
        let len = arg.len() as u64;
        hasher.update(&len.to_le_bytes());
        hasher.update(arg.as_bytes());
    }

    let mut sorted_keys = spec.env_keys.clone();
    sorted_keys.sort();
    for key in &sorted_keys {
        if let Some(val) = std::env::var_os(key) {
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(val.as_encoded_bytes());
            hasher.update(b"\0");
        } else {
            hasher.update(key.as_bytes());
            hasher.update(b"\x01");
        }
    }

    let triple = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    hasher.update(triple.as_bytes());

    // No mount contribution (mounts empty in all existing callers)
    Ok(Digest(*hasher.finalize().as_bytes()))
}

pub fn run_memoized(spec: &RunSpec, store: &Store) -> Result<RunOutcome> {
    // For specs with no mounts, use fast path (no store needed for key)
    let key = if spec.mounts.is_empty() {
        build_key(spec)?
    } else {
        // Validate mount targets before anything else (before hydration)
        for mount in &spec.mounts {
            validate_mount_target(&mount.target)?;
        }
        // assemble_key with hydrate_mounts=false first to check AC hit
        // then hydrate if miss
        assemble_key(spec, store, false)?
    };

    // --- Hit path ---
    if let Ok(Some(record_bytes)) = store.ac_get(&key) {
        if let Some((exit_code, stdout_d, stderr_d)) = decode_ac_record(&record_bytes) {
            let stdout_res = store.get_bytes(&stdout_d);
            let stderr_res = store.get_bytes(&stderr_d);
            if let (Ok(stdout), Ok(stderr)) = (stdout_res, stderr_res) {
                return Ok(RunOutcome {
                    key,
                    hit: true,
                    exit_code,
                    stdout,
                    stderr,
                });
            }
        }
    }

    // --- Miss path ---

    // Hydrate mounts now (only on miss)
    if !spec.mounts.is_empty() {
        for mount in &spec.mounts {
            let dest = spec.cwd.join(&mount.target);
            lightr_index::hydrate(&dest, store, &mount.ref_name)?;
        }
    }

    if spec.command.is_empty() {
        return Err(LightrError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command is empty",
        )));
    }

    let output = std::process::Command::new(&spec.command[0])
        .args(&spec.command[1..])
        .current_dir(&spec.cwd)
        .output()
        .map_err(LightrError::Io)?;

    let exit_code = {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            output
                .status
                .code()
                .unwrap_or_else(|| 128 + output.status.signal().unwrap_or(0))
        }
        #[cfg(not(unix))]
        {
            output.status.code().unwrap_or(1)
        }
    };

    let stdout = output.stdout;
    let stderr = output.stderr;

    if exit_code == 0 && stdout.len() <= OUTPUT_CAP_BYTES && stderr.len() <= OUTPUT_CAP_BYTES {
        let stdout_d = store.put_bytes(&stdout)?;
        let stderr_d = store.put_bytes(&stderr)?;
        let record = encode_ac_record(exit_code, &stdout_d, &stderr_d);
        store.ac_put(&key, &record)?;
    }

    Ok(RunOutcome {
        key,
        hit: false,
        exit_code,
        stdout,
        stderr,
    })
}

/// Compute the memo key and whether the AC already has it — no execution.
pub fn predict(spec: &RunSpec, store: &Store) -> Result<(lightr_core::Digest, bool)> {
    // Validate mount targets (no hydration)
    for mount in &spec.mounts {
        validate_mount_target(&mount.target)?;
    }
    let key = if spec.mounts.is_empty() {
        build_key(spec)?
    } else {
        assemble_key(spec, store, false)?
    };
    let hit = match store.ac_get(&key) {
        Ok(Some(bytes)) => decode_ac_record(&bytes).is_some(),
        _ => false,
    };
    Ok((key, hit))
}

// ---------------------------------------------------------------------------
// R1 — run control types and helpers
// ---------------------------------------------------------------------------

pub struct RunHandle {
    pub id: String,
    pub dir: std::path::PathBuf,
}

pub struct RunInfo {
    pub id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub command: Vec<String>,
    pub created_at_unix: u64,
}

pub enum LogStream {
    Stdout,
    Stderr,
    Both,
}

// ---------------------------------------------------------------------------
// SpecOnDisk — private serde mirror for spec.json
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct MountOnDisk {
    ref_name: String,
    target: String,
}

#[derive(Serialize, Deserialize)]
struct SpecOnDisk {
    cwd: String,
    command: Vec<String>,
    env_keys: Vec<String>,
    mounts: Vec<MountOnDisk>,
    detached: bool,
    created_at_unix: u64,
}

fn read_spec_on_disk(dir: &std::path::Path) -> Result<SpecOnDisk> {
    let bytes = std::fs::read(dir.join("spec.json")).map_err(LightrError::Io)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| LightrError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

fn lightr_home() -> PathBuf {
    if let Ok(h) = std::env::var("LIGHTR_HOME") {
        PathBuf::from(h)
    } else {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));
        home.join(".lightr")
    }
}

fn run_dir_for_id(id: &str) -> PathBuf {
    lightr_home().join("run").join(id)
}

fn new_run_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{nanos}-{pid}")
}

fn write_spec_json(dir: &std::path::Path, spec: &SpecOnDisk) -> Result<()> {
    let bytes = serde_json::to_vec(spec).map_err(|e| LightrError::Io(std::io::Error::other(e)))?;
    std::fs::write(dir.join("spec.json"), &bytes).map_err(LightrError::Io)
}

fn read_pid_file(dir: &std::path::Path) -> Option<i32> {
    std::fs::read_to_string(dir.join("pid"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

fn read_status_file(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join("status"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn parse_exit_code_from_status(status: &str) -> Option<i32> {
    status
        .strip_prefix("exited ")
        .and_then(|s| s.parse::<i32>().ok())
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn ctl_sock_path(dir: &std::path::Path) -> PathBuf {
    dir.join("ctl.sock")
}

fn send_ctl_op(dir: &std::path::Path, op: &str) -> Option<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let sock = ctl_sock_path(dir);
    if !sock.exists() {
        return None;
    }
    let mut stream = UnixStream::connect(&sock).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    stream.write_all(op.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

// ---------------------------------------------------------------------------
// spawn_detached
// ---------------------------------------------------------------------------

pub fn spawn_detached(spec: &RunSpec, _store: &Store) -> Result<RunHandle> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let id = new_run_id();
    let dir = run_dir_for_id(&id);
    std::fs::create_dir_all(&dir).map_err(LightrError::Io)?;

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

    cmd.spawn().map_err(LightrError::Io)?;

    Ok(RunHandle { id, dir })
}

// ---------------------------------------------------------------------------
// supervise
// ---------------------------------------------------------------------------

pub fn supervise(dir: &std::path::Path) -> Result<i32> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    let spec = read_spec_on_disk(dir)?;
    let cwd = PathBuf::from(&spec.cwd);

    // Hydrate mounts (same law as run_memoized)
    // We need a store for hydration — open from LIGHTR_HOME
    let store_root = lightr_home().join("store");
    let store = Store::open(&store_root)?;
    for m in &spec.mounts {
        validate_mount_target(&m.target)?;
        let dest = cwd.join(&m.target);
        lightr_index::hydrate(&dest, &store, &m.ref_name)?;
    }

    // Open log files
    let stdout_log = std::fs::File::create(dir.join("stdout.log")).map_err(LightrError::Io)?;
    let stderr_log = std::fs::File::create(dir.join("stderr.log")).map_err(LightrError::Io)?;

    // Spawn child
    let mut child = std::process::Command::new(&spec.command[0])
        .args(&spec.command[1..])
        .current_dir(&cwd)
        .stdout(std::process::Stdio::from(stdout_log))
        .stderr(std::process::Stdio::from(stderr_log))
        .spawn()
        .map_err(LightrError::Io)?;

    let child_pid = child.id() as i32;

    // Write pid file
    std::fs::write(dir.join("pid"), format!("{child_pid}")).map_err(LightrError::Io)?;

    // Write status = running
    std::fs::write(dir.join("status"), "running").map_err(LightrError::Io)?;

    // Bind ctl.sock
    let sock_path = ctl_sock_path(dir);
    let listener = UnixListener::bind(&sock_path).map_err(LightrError::Io)?;
    listener.set_nonblocking(true).map_err(LightrError::Io)?;

    // Main loop: serve ctl.sock + poll child
    let exit_code = loop {
        // Poll child
        if let Some(status) = child.try_wait().map_err(LightrError::Io)? {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                let code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                break code;
            }
            #[cfg(not(unix))]
            {
                break status.code().unwrap_or(1);
            }
        }

        // Accept ctl connections (non-blocking)
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
                                    #[cfg(unix)]
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

    // Write final status
    std::fs::write(dir.join("status"), format!("exited {exit_code}")).map_err(LightrError::Io)?;

    // Remove ctl.sock
    let _ = std::fs::remove_file(&sock_path);

    Ok(exit_code)
}

// ---------------------------------------------------------------------------
// ps
// ---------------------------------------------------------------------------

pub fn ps(store_home: &std::path::Path) -> Result<Vec<RunInfo>> {
    let run_dir = store_home.join("run");

    if !run_dir.exists() {
        return Ok(vec![]);
    }

    let mut infos: Vec<RunInfo> = Vec::new();

    let entries = std::fs::read_dir(&run_dir).map_err(LightrError::Io)?;
    for entry in entries {
        let entry = entry.map_err(LightrError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Read spec.json
        let spec = match read_spec_on_disk(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Determine running state
        let sock = ctl_sock_path(&path);
        let running = if sock.exists() {
            // Also check pid alive
            if let Some(pid) = read_pid_file(&path) {
                #[cfg(unix)]
                {
                    pid_alive(pid)
                }
                #[cfg(not(unix))]
                {
                    true
                }
            } else {
                false
            }
        } else {
            false
        };

        let exit_code = read_status_file(&path)
            .as_deref()
            .and_then(parse_exit_code_from_status);

        infos.push(RunInfo {
            id,
            running,
            exit_code,
            command: spec.command,
            created_at_unix: spec.created_at_unix,
        });
    }

    // Sort by id descending (newest first — id starts with unix_nanos)
    infos.sort_by(|a, b| b.id.cmp(&a.id));

    Ok(infos)
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

pub fn logs(dir: &std::path::Path, stream: LogStream, follow: bool) -> Result<()> {
    use std::io::Write;

    fn print_file(path: &std::path::Path, offset: &mut u64) -> Result<bool> {
        let data = std::fs::read(path).map_err(LightrError::Io)?;
        let start = *offset as usize;
        if start < data.len() {
            std::io::stdout()
                .write_all(&data[start..])
                .map_err(LightrError::Io)?;
            *offset = data.len() as u64;
            return Ok(true);
        }
        Ok(false)
    }

    let stdout_path = dir.join("stdout.log");
    let stderr_path = dir.join("stderr.log");

    if !follow {
        match stream {
            LogStream::Stdout => {
                let _ = print_file(&stdout_path, &mut 0u64);
            }
            LogStream::Stderr => {
                let _ = print_file(&stderr_path, &mut 0u64);
            }
            LogStream::Both => {
                let _ = print_file(&stdout_path, &mut 0u64);
                let _ = print_file(&stderr_path, &mut 0u64);
            }
        }
        return Ok(());
    }

    // Follow mode
    let mut stdout_off = 0u64;
    let mut stderr_off = 0u64;

    loop {
        let mut had_new = false;
        match stream {
            LogStream::Stdout => {
                if stdout_path.exists() {
                    had_new |= print_file(&stdout_path, &mut stdout_off)?;
                }
            }
            LogStream::Stderr => {
                if stderr_path.exists() {
                    had_new |= print_file(&stderr_path, &mut stderr_off)?;
                }
            }
            LogStream::Both => {
                if stdout_path.exists() {
                    had_new |= print_file(&stdout_path, &mut stdout_off)?;
                }
                if stderr_path.exists() {
                    had_new |= print_file(&stderr_path, &mut stderr_off)?;
                }
            }
        }
        let _ = had_new;

        // Check if exited and no new bytes
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            // Drain any remaining
            let mut drained = false;
            match stream {
                LogStream::Stdout => {
                    if stdout_path.exists() {
                        drained |= print_file(&stdout_path, &mut stdout_off)?;
                    }
                }
                LogStream::Stderr => {
                    if stderr_path.exists() {
                        drained |= print_file(&stderr_path, &mut stderr_off)?;
                    }
                }
                LogStream::Both => {
                    if stdout_path.exists() {
                        drained |= print_file(&stdout_path, &mut stdout_off)?;
                    }
                    if stderr_path.exists() {
                        drained |= print_file(&stderr_path, &mut stderr_off)?;
                    }
                }
            }
            if !drained {
                break;
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// stop
// ---------------------------------------------------------------------------

pub fn stop(dir: &std::path::Path, grace_secs: u64) -> Result<i32> {
    use std::time::{Duration, Instant};

    let sock = ctl_sock_path(dir);

    if sock.exists() {
        // Try sending SIGTERM via ctl.sock
        send_ctl_op(dir, r#"{"op":"signal","sig":15}"#);
    } else if let Some(pid) = read_pid_file(dir) {
        // Direct kill
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }

    // Poll for grace_secs
    let deadline = Instant::now() + Duration::from_secs(grace_secs);
    loop {
        if Instant::now() >= deadline {
            break;
        }
        // Check if already exited
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            if let Some(code) = parse_exit_code_from_status(&status) {
                return Ok(code);
            }
        }
        // Check pid alive
        if let Some(pid) = read_pid_file(dir) {
            #[cfg(unix)]
            {
                if !pid_alive(pid) {
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Check again after grace
    let status = read_status_file(dir).unwrap_or_default();
    if status.starts_with("exited") {
        if let Some(code) = parse_exit_code_from_status(&status) {
            return Ok(code);
        }
    }

    // Still alive — SIGKILL
    if let Some(pid) = read_pid_file(dir) {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    // Wait a bit for status file update
    let kill_deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let status = read_status_file(dir).unwrap_or_default();
        if status.starts_with("exited") {
            if let Some(code) = parse_exit_code_from_status(&status) {
                return Ok(code);
            }
        }
        if std::time::Instant::now() >= kill_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(137)
}

// ---------------------------------------------------------------------------
// exec_in
// ---------------------------------------------------------------------------

pub fn exec_in(dir: &std::path::Path, command: &[String]) -> Result<i32> {
    let spec = read_spec_on_disk(dir)?;
    let cwd = PathBuf::from(&spec.cwd);

    if command.is_empty() {
        return Err(LightrError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command is empty",
        )));
    }

    let mut child = std::process::Command::new(&command[0])
        .args(&command[1..])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(LightrError::Io)?;

    let status = child.wait().map_err(LightrError::Io)?;

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }
    #[cfg(not(unix))]
    {
        Ok(status.code().unwrap_or(1))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lightr_store::Store;
    use std::fs;
    use std::io::Write;

    // LIGHTR_HOME is process-global (index dir): serialize tests and isolate
    // each one in a tempdir home so ~ is never touched.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn isolated_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("LIGHTR_HOME", home.path());
        (home, guard)
    }

    fn make_store(dir: &std::path::Path) -> Store {
        Store::open(dir.join("store")).expect("store open")
    }

    fn make_spec(cwd: &std::path::Path, command: Vec<&str>) -> RunSpec {
        RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: command.into_iter().map(|s| s.to_string()).collect(),
            env_keys: vec![],
            mounts: vec![],
        }
    }

    // -----------------------------------------------------------------------
    // key_stability: same spec twice => same key via two scans
    // -----------------------------------------------------------------------
    #[test]
    fn key_stability() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        // Create a file so the scan has something to digest
        fs::write(cwd.join("file.txt"), b"hello").unwrap();

        let spec = make_spec(cwd, vec!["/bin/echo", "hello"]);
        let k1 = build_key(&spec).expect("key1");
        let k2 = build_key(&spec).expect("key2");
        assert_eq!(k1.0, k2.0, "same spec must produce same key");
    }

    // -----------------------------------------------------------------------
    // key_changes_when_input_file_changes
    // -----------------------------------------------------------------------
    #[test]
    fn key_changes_when_input_file_changes() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        fs::write(cwd.join("data.txt"), b"version1").unwrap();

        let spec = make_spec(cwd, vec!["/bin/echo", "x"]);
        let k1 = build_key(&spec).expect("k1");

        fs::write(cwd.join("data.txt"), b"version2").unwrap();
        let k2 = build_key(&spec).expect("k2");

        assert_ne!(
            k1.0, k2.0,
            "key must change when input file content changes"
        );
    }

    // -----------------------------------------------------------------------
    // key_changes_when_arg_changes
    // -----------------------------------------------------------------------
    #[test]
    fn key_changes_when_arg_changes() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        fs::write(cwd.join("f.txt"), b"data").unwrap();

        let spec1 = make_spec(cwd, vec!["/bin/echo", "argA"]);
        let spec2 = make_spec(cwd, vec!["/bin/echo", "argB"]);

        let k1 = build_key(&spec1).expect("k1");
        let k2 = build_key(&spec2).expect("k2");
        assert_ne!(k1.0, k2.0, "key must change when args change");
    }

    // -----------------------------------------------------------------------
    // key_changes_when_selected_env_changes
    // -----------------------------------------------------------------------
    #[test]
    fn key_changes_when_selected_env_changes() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        fs::write(cwd.join("f.txt"), b"data").unwrap();

        // Env var present
        std::env::set_var("LIGHTR_TEST_VAR_KCW", "valueA");
        let spec1 = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: vec!["/bin/echo".to_string(), "x".to_string()],
            env_keys: vec!["LIGHTR_TEST_VAR_KCW".to_string()],
            mounts: vec![],
        };
        let k1 = build_key(&spec1).expect("k1");

        std::env::set_var("LIGHTR_TEST_VAR_KCW", "valueB");
        let k2 = build_key(&spec1).expect("k2");

        std::env::remove_var("LIGHTR_TEST_VAR_KCW");
        assert_ne!(
            k1.0, k2.0,
            "key must change when selected env value changes"
        );
    }

    // -----------------------------------------------------------------------
    // miss_then_hit: run twice; side-effect file written once; 2nd run is HIT
    // -----------------------------------------------------------------------
    #[test]
    fn miss_then_hit() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        let cwd = work.as_path();
        let store = make_store(tmp.path());

        // Side-effect file outside inputs
        let side_effect = tmp.path().join("side_effect.txt");

        // Command: append one line to side_effect
        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo hit >> {}", side_effect.display()),
        ];

        let spec = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: cmd,
            env_keys: vec![],
            mounts: vec![],
        };

        // First run: miss
        let out1 = run_memoized(&spec, &store).expect("run1");
        assert!(!out1.hit, "first run must be miss");
        assert_eq!(out1.exit_code, 0);

        // Side-effect written once
        let contents1 = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count1 = contents1.lines().count();
        assert_eq!(line_count1, 1, "side effect written once after first run");

        // Second run: hit — command should NOT execute again
        let out2 = run_memoized(&spec, &store).expect("run2");
        assert!(out2.hit, "second run must be hit");
        assert_eq!(out2.exit_code, 0);
        assert_eq!(out1.stdout, out2.stdout, "replayed stdout must match");

        // Side-effect still only 1 line (command did not re-execute)
        let contents2 = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count2 = contents2.lines().count();
        assert_eq!(line_count2, 1, "side effect must not be re-written on hit");
    }

    // -----------------------------------------------------------------------
    // exit_nonzero_never_memoized: exit-7 cmd twice, both MISS, side-effect written twice
    // -----------------------------------------------------------------------
    #[test]
    fn exit_nonzero_never_memoized() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        let cwd = work.as_path();
        let store = make_store(tmp.path());

        let side_effect = tmp.path().join("side_effect_fail.txt");

        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo fail >> {}; exit 7", side_effect.display()),
        ];

        let spec = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: cmd,
            env_keys: vec![],
            mounts: vec![],
        };

        let out1 = run_memoized(&spec, &store).expect("run1");
        assert!(!out1.hit, "first run must be miss");
        assert_eq!(out1.exit_code, 7, "exit code must be 7");

        let out2 = run_memoized(&spec, &store).expect("run2");
        assert!(!out2.hit, "second run must also be miss (not memoized)");
        assert_eq!(out2.exit_code, 7, "exit code must still be 7");

        // Side-effect written twice (command executed both times)
        let contents = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count = contents.lines().count();
        assert_eq!(line_count, 2, "side effect must be written twice");
    }

    // -----------------------------------------------------------------------
    // output_cap_not_memoized: >5MiB stdout not memoized
    // -----------------------------------------------------------------------
    #[test]
    fn output_cap_not_memoized() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        let cwd = work.as_path();
        let store = make_store(tmp.path());

        let side_effect = tmp.path().join("side_effect_cap.txt");

        // Generate >5MiB of stdout output (5 * 1024 * 1024 + 1 bytes)
        // We use a shell one-liner: dd a large file and cat it
        let large_file = tmp.path().join("large.bin");
        {
            // Write 5MiB + 1 byte file
            let mut f = fs::File::create(&large_file).unwrap();
            let buf = vec![b'x'; OUTPUT_CAP_BYTES + 1];
            f.write_all(&buf).unwrap();
        }

        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!(
                "cat {} && echo side >> {}",
                large_file.display(),
                side_effect.display()
            ),
        ];

        let spec = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: cmd,
            env_keys: vec![],
            mounts: vec![],
        };

        // First run: miss (output too large)
        let out1 = run_memoized(&spec, &store).expect("run1");
        assert!(!out1.hit, "first run must be miss");
        assert_eq!(out1.exit_code, 0);
        assert!(
            out1.stdout.len() > OUTPUT_CAP_BYTES,
            "stdout must exceed cap"
        );

        // Second run: also miss (output was not memoized)
        let out2 = run_memoized(&spec, &store).expect("run2");
        assert!(
            !out2.hit,
            "second run must also be miss (output cap exceeded)"
        );

        // Side-effect written twice (command executed both times)
        let contents = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count = contents.lines().count();
        assert_eq!(
            line_count, 2,
            "side effect must be written twice when output cap exceeded"
        );
    }

    // -----------------------------------------------------------------------
    // corrupt_ac_record_treated_as_miss: flip 1 byte in AC record => miss not error
    // -----------------------------------------------------------------------
    #[test]
    fn corrupt_ac_record_treated_as_miss() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        // inputs=[cwd] by law: keep store + side-effects OUTSIDE the input tree
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();
        let cwd = work.as_path();
        let store = make_store(tmp.path());

        let side_effect = tmp.path().join("side_effect_corrupt.txt");

        let cmd = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo ok >> {}", side_effect.display()),
        ];

        let spec = RunSpec {
            cwd: cwd.to_path_buf(),
            inputs: vec![],
            command: cmd,
            env_keys: vec![],
            mounts: vec![],
        };

        // First run: miss, gets memoized
        let out1 = run_memoized(&spec, &store).expect("run1");
        assert!(!out1.hit);
        assert_eq!(out1.exit_code, 0);

        // Build the key to corrupt the AC record directly
        let key = build_key(&spec).expect("key");

        // Read the AC record, corrupt it, write it back
        let record = store.ac_get(&key).expect("ac_get").expect("record present");
        let mut corrupt = record.clone();
        // Flip a byte in the magic to corrupt it
        corrupt[0] ^= 0xFF;
        store.ac_put(&key, &corrupt).expect("ac_put");

        // Third run: corrupt record => miss (not error)
        let out3 = run_memoized(&spec, &store).expect("run3 must not error");
        assert!(!out3.hit, "corrupt AC record must be treated as miss");
        assert_eq!(out3.exit_code, 0);

        // Side-effect written twice (run1 + run3, run2 was just setup)
        let contents = fs::read_to_string(&side_effect).unwrap_or_default();
        let line_count = contents.lines().count();
        assert_eq!(line_count, 2, "command executed on miss and after corrupt");
    }

    // -----------------------------------------------------------------------
    // R1 tests
    // -----------------------------------------------------------------------

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
    // mount_escape_rejected: mount target with ".." rejected
    // -----------------------------------------------------------------------
    #[test]
    fn mount_escape_rejected() {
        let (_home, _env_guard) = isolated_home();
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();

        let err = validate_mount_target("../escape");
        assert!(err.is_err(), "mount target with '..' must be rejected");

        let err2 = validate_mount_target("a/../../escape");
        assert!(
            err2.is_err(),
            "mount target escaping via a/../../ must be rejected"
        );

        // Valid relative target
        assert!(validate_mount_target("subdir").is_ok());
        assert!(validate_mount_target("a/b/c").is_ok());

        // Absolute path rejected
        assert!(validate_mount_target("/abs").is_err());

        let _ = cwd; // suppress unused
    }

    // -----------------------------------------------------------------------
    // mounts_run: run with mount of snapshotted ref → file present in cwd/target
    //             + key changes when mount ref repointed
    // -----------------------------------------------------------------------
    #[test]
    fn mounts_run_and_key_change() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        let tmp = tempfile::tempdir().unwrap();

        // Create a source dir with a file to snapshot
        let src_v1 = tmp.path().join("src_v1");
        fs::create_dir(&src_v1).unwrap();
        fs::write(src_v1.join("hello.txt"), b"hello from v1").unwrap();

        // Snapshot src_v1 as ref "testmount"
        lightr_index::snapshot(&src_v1, &store, "testmount").expect("snapshot v1");

        // Create cwd (separate from input)
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();

        // Run with mount: ref=testmount, target=mounted
        let spec = RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat mounted/hello.txt".to_string(),
            ],
            env_keys: vec![],
            mounts: vec![Mount {
                ref_name: "testmount".to_string(),
                target: "mounted".to_string(),
            }],
        };

        let out1 = run_memoized(&spec, &store).expect("run1 with mount");
        assert_eq!(out1.exit_code, 0, "mounted run should exit 0");
        assert!(
            out1.stdout.starts_with(b"hello from v1"),
            "stdout should contain file content"
        );

        // Verify file was hydrated
        // (After run, the mounted dir should be present — it was hydrated for the miss)
        // Key from first run
        let key1 = out1.key;

        // Now create v2 with different content, re-snapshot to "testmount"
        let src_v2 = tmp.path().join("src_v2");
        fs::create_dir(&src_v2).unwrap();
        fs::write(src_v2.join("hello.txt"), b"hello from v2").unwrap();
        lightr_index::snapshot(&src_v2, &store, "testmount").expect("snapshot v2");

        // Remove the mounted dir so hydrate can succeed again
        let mounted_dir = work.join("mounted");
        if mounted_dir.exists() {
            fs::remove_dir_all(&mounted_dir).unwrap();
        }

        // Re-run: key must change because mount ref's root digest changed
        let out2 = run_memoized(&spec, &store).expect("run2 with mount v2");
        assert_eq!(out2.exit_code, 0);
        assert!(
            out2.stdout.starts_with(b"hello from v2"),
            "stdout should contain v2 content"
        );
        assert_ne!(
            key1, out2.key,
            "key must change when mount ref is repointed"
        );
    }

    // -----------------------------------------------------------------------
    // predict: miss → run → predict hit
    // -----------------------------------------------------------------------
    #[test]
    fn predict_miss_run_hit() {
        let (home, _env_guard) = isolated_home();
        let home_path = home.path().to_path_buf();
        let store = make_store(&home_path);

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        fs::create_dir(&work).unwrap();

        let spec = RunSpec {
            cwd: work.clone(),
            inputs: vec![],
            command: vec!["/bin/echo".to_string(), "predict-test".to_string()],
            env_keys: vec![],
            mounts: vec![],
        };

        // predict before run: should be miss
        let (key1, hit1) = predict(&spec, &store).expect("predict1");
        assert!(!hit1, "predict before run must be miss");

        // Run it
        let out = run_memoized(&spec, &store).expect("run");
        assert!(!out.hit, "first run must be miss");
        assert_eq!(out.key, key1, "predict key must match run key");

        // predict after run: should be hit
        let (key2, hit2) = predict(&spec, &store).expect("predict2");
        assert_eq!(key1, key2, "key must be stable");
        assert!(hit2, "predict after run must be hit");
    }
}

// ---------------------------------------------------------------------------
// R4 additions — frozen contract: build-spec-r4.md §1 (bodies: R4-W1)
// ---------------------------------------------------------------------------

/// Deep-memo (opt-in nitro, ADR-0016): process-tree memoization via a
/// spawn-shim. Degrades HONESTLY to whole-run memo when the shim can't
/// attach (SIP/static binaries) — never silently claims the capability.
pub struct DeepMemoConfig {
    pub enabled: bool,
}

/// run_memoized with optional deep-memo. When cfg.enabled and the shim
/// attaches, sub-invocations are memoized; otherwise falls back to whole-run
/// memo (run_memoized) and reports the fallback reason via the returned
/// outcome's stderr stream / a stderr note at the CLI layer.
pub fn run_memoized_deep(
    _spec: &RunSpec,
    _store: &Store,
    _cfg: &DeepMemoConfig,
) -> Result<RunOutcome> {
    todo!("R4-W1: spawn-shim deep-memo + honest fallback to run_memoized")
}
