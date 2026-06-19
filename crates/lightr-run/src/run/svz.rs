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
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

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
    {
        let vm_done = Arc::clone(&vm_done);
        let vm_code = Arc::clone(&vm_code);
        let rootfs_dir = rootfs_dir.clone();
        let cwd = cwd.clone();
        std::thread::spawn(move || {
            let code = match engine_for(EngineKind::Vz) {
                Ok(engine) => {
                    let spec = ExecSpec {
                        cwd: &cwd,
                        command: &command,
                        rootfs: Some(&rootfs_dir),
                        limits: lightr_core::ResourceLimits::default(),
                        net: true,
                        // ADR-0018 dual-NIC: the L2 switch (WP-C9) will assign the
                        // guest-side socketpair fd here to attach the mesh NIC
                        // (eth1) alongside the NAT NIC (eth0). Until that lands,
                        // None keeps today's single-NAT-NIC behavior unchanged.
                        net_fd: None,
                        net_mac: None,
                    };
                    engine.run(&spec).unwrap_or(255)
                }
                Err(_) => 255, // vz unavailable (non-macOS / no pack) → honest non-zero
            };
            *vm_code.lock().expect("vm_code mutex") = code;
            vm_done.store(true, Ordering::SeqCst);
        });
    }

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
        return Ok(code);
    };

    // 4. Live: write our pid (stop()'s SIGKILL fallback kills us → the in-process
    //    VM dies with us) + status, then forward each published port to the guest.
    std::fs::write(dir.join("pid"), format!("{}", std::process::id())).map_err(LightrError::Io)?;
    std::fs::write(dir.join("status"), "running").map_err(LightrError::Io)?;

    // 5. A forwarder per published port → the guest IP. A bind failure is logged
    //    and skipped (a port clash on one publish must not down the whole run),
    //    exactly like the native path. Held until the loop exits, then dropped.
    let mut forwarders: Vec<crate::portforward::Forwarder> = Vec::new();
    for &(host_port, container_port) in &spec.ports {
        match crate::portforward::start_to(host_port, &guest_ip, container_port) {
            Ok(fwd) => forwarders.push(fwd),
            Err(e) => {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("stderr.log"))
                {
                    let _ = writeln!(
                        f,
                        "lightr: publish 127.0.0.1:{host_port} -> {guest_ip}:{container_port} failed: {e}"
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
