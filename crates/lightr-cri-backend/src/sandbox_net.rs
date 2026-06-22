//! cfg(linux) netns + CNI executor — TRANSCRIBED from `lightr-cri-net`
//! (netns.rs + chain.rs). This whole file compiles ONLY on linux (gated at the
//! `mod net` site in sandbox.rs); macOS/windows never see it.
//!
//! NOT a crate dependency on lightr-cri-net (ADR-0017 firewall) — the syscalls
//! are issued directly via `libc` so the crate adds no `nix` dep. The shape is
//! the same: bind-mount netns pin (R0 = stateless, NO pause-holder process per
//! LEAD decision — the mount pins the kernel ns; honors no-daemon),
//! umount2(MNT_DETACH)-then-unlink teardown LAW, forward CNI ADD / reverse DEL
//! with prevResult threading and portMappings runtimeConfig.
//!
//! RUNTIME VALIDATION: this path is exercised only on Linux CI / on-box; it is
//! NOT verifiable on the macOS gate (contract §5). The pure helpers
//! (`derive_plugin_config`, `extract_first_ipv4`, conflist parsing) ARE
//! host-testable and carry unit tests.

use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

use crate::vocab::{PortMapping, Protocol, SandboxId};

const NETNS_DIR: &str = "/run/netns";

/// Resolved CNI config: the dir holding the chosen conflist + the plugin bin dir.
pub(crate) struct CniEnv {
    conf_dir: PathBuf,
    bin_dir: PathBuf,
}

/// Probe whether CNI is available AND the process is privileged enough to
/// create a netns. Probe-truthful: returns None (host-network fallback) when no
/// conflist / bin dir, or when `unshare(CLONE_NEWNET)` is denied (EPERM).
pub(crate) fn cni_available() -> Option<CniEnv> {
    let conf_dir = std::env::var("LIGHTR_CNI_CONF")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/cni/net.d"));
    let bin_dir = std::env::var("LIGHTR_CNI_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/cni/bin"));

    let has_conflist = conf_dir
        .read_dir()
        .ok()?
        .filter_map(|e| e.ok())
        .any(|e| is_conflist(&e.path()));
    if !has_conflist || !bin_dir.is_dir() {
        return None;
    }

    // Privilege probe on a throwaway thread (unshare mutates only that thread).
    let priv_ok =
        std::thread::spawn(|| -> bool { unsafe { libc::unshare(libc::CLONE_NEWNET) == 0 } })
            .join()
            .unwrap_or(false);
    if !priv_ok {
        return None;
    }
    Some(CniEnv { conf_dir, bin_dir })
}

fn is_conflist(p: &Path) -> bool {
    p.extension().and_then(|x| x.to_str()) == Some("conflist")
}

/// Create+pin a netns and run the CNI chain → (pod_ip, netns_path_string).
pub(crate) fn setup(
    id: &SandboxId,
    env: &CniEnv,
    port_mappings: &[PortMapping],
) -> io::Result<(Option<String>, String)> {
    // Name the netns after the sandbox id (first 24 chars → IFNAMSIZ headroom).
    let ns_name = format!("lightr-{}", &id.0[..id.0.len().min(24)]);
    let ns_path = netns_create(&ns_name)?;
    let ns_path_str = ns_path.to_string_lossy().into_owned();

    let conflist = first_conflist(&env.conf_dir)
        .ok_or_else(|| io::Error::other(format!("no .conflist in {}", env.conf_dir.display())))?;
    let ip = chain_add(&conflist, &id.0, &ns_path, &env.bin_dir, port_mappings)
        .map_err(|e| io::Error::other(format!("CNI ADD: {e}")))?;
    Ok((ip, ns_path_str))
}

/// CNI DEL then netns teardown. Fail-closed: DEL errors are logged, teardown
/// continues (the umount+unlink LAW must run so the kernel ns is freed).
pub(crate) fn teardown(id: &SandboxId, netns_path: &str) {
    let ns_path = Path::new(netns_path);
    if let Some(env) = cni_available() {
        if let Some(conflist) = first_conflist(&env.conf_dir) {
            if let Err(e) = chain_del(&conflist, &id.0, ns_path, &env.bin_dir) {
                eprintln!("[lightr-cri] CNI DEL sandbox {} (continuing): {e}", id.0);
            }
        }
    }
    if let Err(e) = netns_teardown(ns_path) {
        eprintln!("[lightr-cri] netns teardown {netns_path}: {e}");
    }
}

// ── netns lifecycle (libc unshare + mount bind-pin + umount2/unlink) ──────────

/// Create a named netns pinned at `/run/netns/<name>` (containerd pattern). The
/// `unshare(CLONE_NEWNET)` + bind-mount run on a DEDICATED thread so only that
/// thread's netns is mutated; the mount pins the ns after the thread exits.
fn netns_create(name: &str) -> io::Result<PathBuf> {
    let dir = Path::new(NETNS_DIR);
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;

    let path_c = path.clone();
    std::thread::spawn(move || -> io::Result<()> { netns_create_on_thread(&path_c) })
        .join()
        .map_err(|_| io::Error::other("netns create thread panicked"))??;
    Ok(path)
}

fn netns_create_on_thread(path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if unsafe { libc::unshare(libc::CLONE_NEWNET) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    let src = format!("/proc/self/task/{tid}/ns/net");
    let src_c = CString::new(src).map_err(|_| io::Error::other("nul in netns src"))?;
    let dst_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("nul in netns path"))?;
    // bind-mount the thread-local ns file onto the pinned path.
    let rc = unsafe {
        libc::mount(
            src_c.as_ptr(),
            dst_c.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::other(format!(
            "bind-mount netns: {}",
            io::Error::last_os_error()
        )));
    }
    // Bring loopback UP inside the fresh netns (a new netns has `lo` DOWN;
    // in-netns 127.0.0.1 dials would otherwise time out). Non-fatal.
    let _ = std::process::Command::new("ip")
        .args(["link", "set", "lo", "up"])
        .status();
    Ok(())
}

/// LAW: umount2(MNT_DETACH) THEN unlink — reversing causes EBUSY and leaks the
/// kernel ns (containerd#6143).
fn netns_teardown(path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let p_c =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::other("nul in path"))?;
    if unsafe { libc::umount2(p_c.as_ptr(), libc::MNT_DETACH) } != 0 {
        return Err(io::Error::other(format!(
            "umount2: {}",
            io::Error::last_os_error()
        )));
    }
    std::fs::remove_file(path)
}

/// Open a pinned netns `O_RDONLY` for a `setns(CLONE_NEWNET)` in a child's
/// `pre_exec` (the container-join path, called from container.rs).
pub(crate) fn join_netns(path: &Path) -> io::Result<std::os::unix::io::OwnedFd> {
    use std::os::unix::io::{FromRawFd, IntoRawFd, OwnedFd};
    let f = std::fs::OpenOptions::new().read(true).open(path)?;
    let raw = f.into_raw_fd();
    // SAFETY: `f` was open + valid; into_raw_fd hands sole ownership to OwnedFd.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

// ── CNI chain (exec plugins, forward ADD / reverse DEL) ───────────────────────

fn chain_add(
    conflist_path: &Path,
    container_id: &str,
    netns_path: &Path,
    bin_dir: &Path,
    port_mappings: &[PortMapping],
) -> io::Result<Option<String>> {
    let conflist = load_conflist(conflist_path)?;
    let version = conflist_str(&conflist, "cniVersion", "1.0.0");
    let name = conflist_str(&conflist, "name", "");
    let plugins = conflist_plugins(&conflist)?;
    let mut prev: Option<Value> = None;
    for plugin in &plugins {
        let ptype = plugin_type(plugin)?;
        let stdin = derive_plugin_config(plugin, &version, &name, prev.as_ref(), port_mappings);
        let out = exec_plugin(bin_dir, &ptype, "ADD", container_id, netns_path, &stdin)?;
        prev = Some(serde_json::from_str(&out).map_err(|e| io::Error::other(e.to_string()))?);
    }
    Ok(prev.as_ref().and_then(extract_first_ipv4))
}

fn chain_del(
    conflist_path: &Path,
    container_id: &str,
    netns_path: &Path,
    bin_dir: &Path,
) -> io::Result<()> {
    let conflist = load_conflist(conflist_path)?;
    let version = conflist_str(&conflist, "cniVersion", "1.0.0");
    let name = conflist_str(&conflist, "name", "");
    let plugins = conflist_plugins(&conflist)?;
    let mut last_err = None;
    for plugin in plugins.iter().rev() {
        let Ok(ptype) = plugin_type(plugin) else {
            continue;
        };
        let stdin = derive_plugin_config(plugin, &version, &name, None, &[]);
        if let Err(e) = exec_plugin(bin_dir, &ptype, "DEL", container_id, netns_path, &stdin) {
            eprintln!("[lightr-cri] CNI DEL plugin {ptype} (continuing): {e}");
            last_err = Some(e);
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn first_conflist(conf_dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = conf_dir
        .read_dir()
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_conflist(p))
        .collect();
    entries.sort();
    entries.into_iter().next()
}

fn load_conflist(path: &Path) -> io::Result<Value> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| io::Error::other(e.to_string()))
}

fn conflist_str(v: &Value, key: &str, default: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or(default)
        .to_string()
}

fn conflist_plugins(v: &Value) -> io::Result<Vec<Value>> {
    v.get("plugins")
        .and_then(|p| p.as_array())
        .map(|a| a.to_vec())
        .ok_or_else(|| io::Error::other("conflist missing 'plugins' array"))
}

fn plugin_type(plugin: &Value) -> io::Result<String> {
    plugin
        .get("type")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| io::Error::other("plugin missing 'type'"))
}

/// Build per-plugin stdin JSON: own object + injected cniVersion/name +
/// (ADD only) prevResult + runtimeConfig.portMappings for declared cap.
fn derive_plugin_config(
    plugin: &Value,
    version: &str,
    name: &str,
    prev: Option<&Value>,
    port_mappings: &[PortMapping],
) -> String {
    let mut obj = plugin.as_object().cloned().unwrap_or_default();
    obj.insert("cniVersion".into(), Value::String(version.into()));
    obj.insert("name".into(), Value::String(name.into()));
    if let Some(p) = prev {
        obj.insert("prevResult".into(), p.clone());
    }
    let has_cap = plugin
        .get("capabilities")
        .and_then(|c| c.get("portMappings"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if has_cap && !port_mappings.is_empty() {
        // host_port == 0 means "no host mapping" — omit (portmap rejects 0).
        let pms: Vec<Value> = port_mappings
            .iter()
            .filter(|pm| pm.host_port > 0)
            .map(serialize_port_mapping)
            .collect();
        if !pms.is_empty() {
            let mut rc = serde_json::Map::new();
            rc.insert("portMappings".into(), Value::Array(pms));
            obj.insert("runtimeConfig".into(), Value::Object(rc));
        }
    }
    serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|_| "{}".into())
}

fn serialize_port_mapping(pm: &PortMapping) -> Value {
    let proto = match pm.protocol {
        Protocol::Tcp => "tcp",
        Protocol::Udp => "udp",
        Protocol::Sctp => "sctp",
    };
    serde_json::json!({
        "hostPort": pm.host_port,
        "containerPort": pm.container_port,
        "protocol": proto,
        "hostIP": pm.host_ip,
    })
}

/// First IPv4 from a CNI result's `ips[]`, prefix stripped (`10.88.0.5/16` →
/// `10.88.0.5`).
fn extract_first_ipv4(result: &Value) -> Option<String> {
    let ips = result.get("ips")?.as_array()?;
    for entry in ips {
        let addr = entry.get("address").and_then(|a| a.as_str())?;
        if !addr.contains(':') {
            return Some(addr.split('/').next().unwrap_or(addr).to_string());
        }
    }
    None
}

fn parse_cni_error(stdout: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(stdout) {
        if let Some(m) = v.get("msg").and_then(|m| m.as_str()) {
            return m.to_string();
        }
        if let Some(m) = v.get("details").and_then(|m| m.as_str()) {
            return m.to_string();
        }
    }
    stdout.to_string()
}

fn exec_plugin(
    bin_dir: &Path,
    plugin_type: &str,
    command: &str,
    container_id: &str,
    netns: &Path,
    stdin_json: &str,
) -> io::Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let binary = bin_dir.join(plugin_type);
    let mut child = Command::new(&binary)
        .env("CNI_COMMAND", command)
        .env("CNI_CONTAINERID", container_id)
        .env("CNI_NETNS", netns.to_str().unwrap_or(""))
        .env("CNI_IFNAME", "eth0")
        .env("CNI_PATH", bin_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut s) = child.stdin.take() {
        s.write_all(stdin_json.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|e| io::Error::other(e.to_string()))
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = if stdout.is_empty() {
            stderr.to_string()
        } else {
            parse_cni_error(&stdout)
        };
        Err(io::Error::other(format!("{plugin_type}: {msg}")))
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "sandbox_net_tests.rs"]
mod tests;
