//! supervise_vz (WP-NET2) — detached vz container with host→guest port forwarding.
//! unix impl + non-unix stub.

use lightr_core::{LightrError, Result};
use lightr_store::Store;

#[cfg(unix)]
use super::ctl::ctl_sock_path;
use super::types::SpecOnDisk;

/// Supervise a `vz` container run: boot a Linux microVM in THIS process and
/// forward each published port (`127.0.0.1:host` → `guest_ip:container`) to the
/// guest's DHCP IP. This is the `-p`-for-a-Linux-image case.
///
/// Lifecycle:
/// 1. Hydrate the rootfs ref CoW into `<run_dir>/rootfs` (lives for the VM, gc'd
///    with the run dir).
/// 2. Boot the VM on a worker thread — `engine.run(net=true)` blocks until the VM
///    stops. `net=true` makes the engine attach the NAT NIC (`ip=dhcp`) and the
///    guest publish its IP to `IP_FILE`.
/// 3. Read the guest IP from `IP_FILE` (or bail if the VM exits first).
/// 4. Write pid (our own) + status, start a forwarder per published port.
/// 5. Serve `ctl.sock` (status/signal) + poll the VM. `signal` writes the guest
///    `EXIT_FILE` with the `128+sig` code; the shim polls it and force-stops the
///    VM (no new shim code), the worker returns, and we exit cleanly.
///
/// Stop semantics: `stop` sends `signal` via `ctl.sock` (→ force-stop, clean
/// status), with the usual pid-SIGKILL fallback — and since the VM runs IN this
/// process, killing the supervisor tears the VM down too.
#[cfg(unix)]
pub(super) fn supervise_vz(dir: &std::path::Path, spec: &SpecOnDisk, store: &Store) -> Result<i32> {
    use lightr_engine::{engine_for, EngineKind, ExecSpec};
    use lightr_init::{EXIT_FILE, IP_FILE};
    use std::io::{BufRead, BufReader, Write};
    use std::net::Ipv4Addr;
    use std::os::unix::io::{AsRawFd, OwnedFd};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    // WP-C9: the live mesh attach for a `--network` vz run — the owned guest fd
    // (kept open for the VM's life) plus the registry-assigned identity threaded
    // into ExecSpec. `None` for a run with no `--network`.
    struct MeshAttach {
        network: String,
        member_name: String,
        guest_fd: OwnedFd,
        mac: [u8; 6],
        ip: Ipv4Addr,
    }

    let rootfs_ref = spec
        .rootfs_ref
        .clone()
        .ok_or_else(|| LightrError::InvalidRef("vz supervise: missing rootfs_ref".to_string()))?;
    let cwd = PathBuf::from(&spec.cwd);

    // 1. Hydrate the rootfs ref into <run_dir>/rootfs (persists for the VM's life;
    //    cleaned with the run dir, unlike the memo path's throwaway temp dir).
    let rootfs_dir = dir.join("rootfs");
    std::fs::create_dir_all(&rootfs_dir).map_err(LightrError::Io)?;
    lightr_index::hydrate(&rootfs_dir, store, &rootfs_ref)?;

    // The guest's durable EXIT_FILE + IP_FILE on the share. Writing EXIT_FILE
    // force-stops the VM (the shim polls it); IP_FILE is where the guest publishes
    // its DHCP IP. Both paths agree with the engine/guest by construction (rootfs
    // dir + the lightr_init const), so they can never drift.
    let exit_file = rootfs_dir.join(EXIT_FILE.trim_start_matches('/'));
    let ip_file = rootfs_dir.join(IP_FILE.trim_start_matches('/'));

    // 2. Boot the VM on a worker thread (engine.run blocks until the VM stops).
    //    Safety of env mutation inside engine.run: VzEngine::run sets LIGHTR_VZ_NET
    //    / LIGHTR_VZ_EXITFILE ONCE at the very start of run() — before the VM boot
    //    FFI call — at which point the only other live thread is this main thread,
    //    polling IP_FILE via std::fs (no getenv). The forwarder + ctl threads do
    //    not exist yet (they start in step 4, ~1–2s later after the IP appears), so
    //    no thread reads the environment concurrently with those set_var calls.
    let vm_done = Arc::new(AtomicBool::new(false));
    let vm_code = Arc::new(Mutex::new(255i32));
    let command = spec.command.clone();

    // WP-C9 (ADR-0018): if this vz run joins a `--network`, create-or-open the
    // per-network registry, JOIN it (deterministic MAC + mesh IP), and ATTACH the
    // shared cross-process L2 switch — returning the GUEST end of the mesh NIC
    // (`eth1`). The guest fd must outlive the whole VM (the vz shim wraps it
    // non-owning), so the `OwnedFd` is MOVED into the worker closure and dropped
    // only when the VM stops. `spec.network == None` ⇒ none of this runs and the
    // ExecSpec below is byte-identical to the single-NAT-NIC path shipped today.
    let home = super::paths::lightr_home();
    let mesh: Option<MeshAttach> = if let Some(net) = spec.network.clone() {
        let reg = crate::network::NetworkRegistry::create(&home, &net).map_err(LightrError::Io)?;
        // The member's switch identity is its run name (`--name`/`--network-alias`
        // are its DNS aliases). Fall back to the run-dir id when unnamed so each
        // member is a distinct registry record.
        let member_name = spec.name.clone().unwrap_or_else(|| {
            dir.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("vz")
                .to_string()
        });
        let ports: Vec<(u16, u16)> = spec.ports.clone();
        let member = reg
            .join(&member_name, &spec.network_alias, &ports)
            .map_err(LightrError::Io)?;
        let guest_fd =
            crate::vswitch::switch_host::attach(&home, &net, &member).map_err(LightrError::Io)?;
        Some(MeshAttach {
            network: net,
            member_name,
            guest_fd,
            mac: member.mac.0,
            ip: member.ip,
        })
    } else {
        None
    };
    // Borrow-stable copies for the worker closure (the `OwnedFd` is moved in).
    let mesh_mac = mesh.as_ref().map(|m| m.mac);
    let mesh_ip = mesh.as_ref().map(|m| m.ip);
    let mesh_fd = mesh.as_ref().map(|m| m.guest_fd.as_raw_fd());
    // Detach identity (network id + member name) kept in THIS thread so the exit
    // paths can `switch_host::detach` after the worker (holding the OwnedFd) ends.
    let detach_id: Option<(String, String)> = mesh
        .as_ref()
        .map(|m| (m.network.clone(), m.member_name.clone()));
    // `--add-host host:ip` → (host, ip) pairs; `--dns`/`--hostname` carried as-is.
    let add_host: Vec<(String, String)> = spec.add_host.clone();
    let dns: Vec<String> = spec.dns.clone();
    let hostname: Option<String> = spec.hostname.clone();
    // WP-RESLIMITS: read the persisted resource caps back from spec.json so the
    // VM is sized to them (`vz_caps`: a hard memory cap + ceil(cpus) vcpus). Both
    // `None` (unlimited) ⇒ the shim baseline, byte-identical to before.
    let limits = lightr_core::ResourceLimits {
        memory_bytes: spec.mem_limit_bytes,
        cpu_millis: spec.cpu_limit_millis,
        // vz cannot set a guest per-container pids.max; a pids request is honest-
        // errored at the CLI before a detached vz run is ever spawned ⇒ never set.
        pids_max: None,
    };
    {
        let vm_done = Arc::clone(&vm_done);
        let vm_code = Arc::clone(&vm_code);
        let rootfs_dir = rootfs_dir.clone();
        let cwd = cwd.clone();
        // Move the mesh attach (the `OwnedFd`) into the worker so the guest fd
        // stays open for the whole VM lifetime (the vz shim wraps it non-owning);
        // it is dropped when this thread ends, after the VM stops.
        let _mesh_keepalive = mesh;
        std::thread::spawn(move || {
            // Keep the guest fd alive for the duration of engine.run.
            let _mesh_keepalive = _mesh_keepalive;
            let code = match engine_for(EngineKind::Vz) {
                Ok(engine) => {
                    let spec = ExecSpec {
                        cwd: &cwd,
                        command: &command,
                        rootfs: Some(&rootfs_dir),
                        limits,
                        net: true,
                        net_isolate: false,
                        // ADR-0018 dual-NIC: when this run joined a `--network`,
                        // `mesh_fd` is the guest end of the mesh NIC (eth1) the L2
                        // switch owns the host end of; eth0 (NAT egress) is
                        // unchanged. `None` ⇒ today's single-NAT-NIC path.
                        net_fd: mesh_fd,
                        net_mac: mesh_mac,
                        mounts: &[],
                        env: &[],
                        workdir: None,
                        user: None,
                        hostname: hostname.as_deref(),
                        add_host: &add_host,
                        dns: &dns,
                        mesh_ip,
                        // WP-#92: the vz supervisor path is a microVM, not the ns
                        // engine; --read-only/--shm-size are ns-enforced. Defaults.
                        read_only: false,
                        shm_size: None,
                        // WP-#94: the vz supervisor path is a microVM; caps are an
                        // ns-engine concern. Defaults (no cap changes).
                        cap_drop: &[],
                        cap_add: &[],
                        init: false,
                        join_netns: None,
                        cgroup_name: None,
                        // WP-#102: vz supervisor is a microVM; no exec-readiness pipe.
                        exec_ready_fd: None,
                    };
                    engine.run(&spec).unwrap_or(255)
                }
                Err(_) => 255, // vz unavailable (non-macOS / no pack) → honest non-zero
            };
            *vm_code.lock().expect("vm_code mutex") = code;
            vm_done.store(true, Ordering::SeqCst);
        });
    }

    // WP-C9: on EVERY exit path of a `--network` run, leave the registry; the
    // switch host's refcount self-watch then self-stops when the last member is
    // gone. Best-effort (a detach failure must not mask the run's exit code).
    let detach = {
        let home = home.clone();
        let detach_id = detach_id.clone();
        move || {
            if let Some((net, name)) = &detach_id {
                let _ = crate::vswitch::switch_host::detach(&home, net, name);
            }
        }
    };

    // 3. Wait for the guest IP (boot + kernel DHCP, ~1–2s) OR an early VM exit
    //    (boot failure / instant command exit). Generous deadline for a cold boot.
    let ip_deadline = Instant::now() + Duration::from_secs(60);
    let guest_ip: Option<String> = loop {
        if let Ok(s) = std::fs::read_to_string(&ip_file) {
            let ip = s.trim().to_string();
            if !ip.is_empty() {
                break Some(ip);
            }
        }
        if vm_done.load(Ordering::SeqCst) {
            break None; // VM stopped before publishing an IP
        }
        if Instant::now() >= ip_deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    // No IP ⇒ the run is not networkable: record the VM's (final) exit code and
    // stop. Force-stop best-effort in case the VM is up but networking failed.
    let Some(guest_ip) = guest_ip else {
        for _ in 0..20 {
            if vm_done.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::write(&exit_file, "143");
        let code = *vm_code.lock().expect("vm_code mutex");
        let _ = std::fs::write(dir.join("status"), format!("exited {code}"));
        detach();
        return Ok(code);
    };

    // 4. Live: write our pid (stop()'s SIGKILL fallback kills us → the in-process
    //    VM dies with us) + status, then forward each published port to the guest.
    std::fs::write(dir.join("pid"), format!("{}", std::process::id())).map_err(LightrError::Io)?;
    std::fs::write(dir.join("status"), "running").map_err(LightrError::Io)?;

    // 5. A forwarder per published port → the guest IP. A bind failure is logged
    //    and skipped (a port clash on one publish must not down the whole run),
    //    exactly like the native path. Held until the loop exits, then dropped.
    // WP-B2: bind each published port on its requested host interface (Docker
    // `-p HOST_IP:H:C`), preferring the go-forward `ports2` channel; the forward
    // TARGET is the guest IP (the container's server). Empty host_ip ⇒ `0.0.0.0`.
    let mut forwarders: Vec<crate::portforward::Forwarder> = Vec::new();
    for (host_ip, host_port, container_port) in spec.published_ports() {
        let bind_ip = if host_ip.is_empty() {
            "0.0.0.0"
        } else {
            host_ip.as_str()
        };
        match crate::portforward::start_on(bind_ip, host_port, &guest_ip, container_port) {
            Ok(fwd) => forwarders.push(fwd),
            Err(e) => {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("stderr.log"))
                {
                    let _ = writeln!(
                        f,
                        "lightr: publish {bind_ip}:{host_port} -> {guest_ip}:{container_port} failed: {e}"
                    );
                }
            }
        }
    }

    // 6. ctl.sock loop: serve status/signal + poll the VM (mirrors the unix native
    //    loop). `signal` writes EXIT_FILE (force-stop); the shim stops the VM, the
    //    worker returns, vm_done flips, and we break with the real exit code.
    let sock_path = ctl_sock_path(dir);
    let listener = UnixListener::bind(&sock_path).map_err(LightrError::Io)?;
    listener.set_nonblocking(true).map_err(LightrError::Io)?;

    let exit_code = loop {
        if vm_done.load(Ordering::SeqCst) {
            break *vm_code.lock().expect("vm_code mutex");
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
                                // Force-stop: write the guest EXIT_FILE with the
                                // 128+signal code; the shim polls it and stops the
                                // VM. Default sig 15 (SIGTERM ⇒ 143). The VM is
                                // in-process, so this is how the supervisor relays
                                // a "stop" into the guest's force-teardown.
                                let sig = req.get("sig").and_then(|v| v.as_i64()).unwrap_or(15);
                                let code = 128 + sig as i32;
                                let _ = std::fs::write(&exit_file, format!("{code}"));
                                serde_json::json!({"ok": true})
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

    std::fs::write(dir.join("status"), format!("exited {exit_code}")).map_err(LightrError::Io)?;
    let _ = std::fs::remove_file(&sock_path);
    drop(forwarders); // close listeners + per-connection threads
    detach();
    Ok(exit_code)
}

/// Non-unix stub: the `vz` engine is macOS-only (unix). On a non-unix host a vz
/// run never reaches here (the CLI won't route it), but the symbol must exist for
/// `supervise`'s unconditional call to compile. Fails closed.
#[cfg(not(unix))]
pub(super) fn supervise_vz(
    _dir: &std::path::Path,
    _spec: &SpecOnDisk,
    _store: &Store,
) -> Result<i32> {
    Err(LightrError::InvalidRef(
        "vz supervise requires a unix host (macOS)".to_string(),
    ))
}
