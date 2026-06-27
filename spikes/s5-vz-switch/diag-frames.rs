//! DIAG (ADR-0018 WP-C10): isolate the file-handle mesh NIC TRANSPORT from the
//! switch logic. Boots ONE alpine VM with eth1 on a raw socketpair (NO VSwitch),
//! and dumps every datagram the HOST end receives while the guest brings eth1 up
//! and runs udhcpc on it. THROWAWAY.
//!
//! Verdict logic:
//!   * frames arrive on the host fd  ⇒ transport OK; bug is in vswitch/dhcp.rs
//!     (parse/build) or the switch reply path. We decode the first DISCOVER so
//!     the lead can compare the on-wire bytes vs dhcp::handle's expectations.
//!   * NO frames arrive            ⇒ transport bug (engine net_fd path / vz.swift
//!     file-handle NIC / fd lifetime) — the guest's eth1 TX never reaches the
//!     host socket.

use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lightr_core::ResourceLimits;
use lightr_engine::{engine_for, EngineKind, ExecSpec};
use lightr_store::Store;

const ALPINE_REF: &str = "alpine";

fn datagram_socketpair() -> (RawFd, RawFd) {
    let mut fds = [0 as libc::c_int; 2];
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "socketpair: {}", std::io::Error::last_os_error());
    (fds[0], fds[1])
}

fn hexdump(b: &[u8], max: usize) -> String {
    let n = b.len().min(max);
    let mut s = String::new();
    for (i, x) in b[..n].iter().enumerate() {
        if i % 16 == 0 {
            s.push_str(&format!("\n      {i:04x}: "));
        }
        s.push_str(&format!("{x:02x} "));
    }
    s
}

fn main() {
    let scratch = std::env::temp_dir().join(format!("s5-diag-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let store_root = Store::default_root();
    eprintln!("DIAG-FRAMES: store={} scratch={}", store_root.display(), scratch.display());

    let (host_fd, guest_fd) = datagram_socketpair();

    // Read raw datagrams off the host end on a thread; count + decode.
    let host = unsafe { UnixDatagram::from_raw_fd(host_fd) };
    host.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reader = {
        let stop = Arc::clone(&stop);
        let count = Arc::clone(&count);
        thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            let mut first_discover_dumped = false;
            loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                match host.recv(&mut buf) {
                    Ok(0) => continue,
                    Ok(n) => {
                        let c = count.fetch_add(1, Ordering::SeqCst) + 1;
                        let f = &buf[..n];
                        // Classify the frame.
                        let kind = classify(f);
                        eprintln!("  RX#{c} len={n} {kind}");
                        // Dump the first UDP/68->67 (DHCP DISCOVER) in full-ish.
                        if !first_discover_dumped && is_dhcp_to_server(f) {
                            first_discover_dumped = true;
                            eprintln!(
                                "  >>> first guest DHCP frame (eth/ip/udp/bootp), {} bytes:{}",
                                n,
                                hexdump(f, 300)
                            );
                        }
                    }
                    Err(e) => match e.kind() {
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => continue,
                        _ => break,
                    },
                }
            }
        })
    };

    // Boot one VM: bring eth1 up, send a few DISCOVERs, also ping-broadcast +
    // arping so we generate non-DHCP frames too (proves any TX crosses).
    let store = Store::open(&store_root).unwrap();
    let rootfs_dir = scratch.join("rootfs-a");
    std::fs::create_dir_all(&rootfs_dir).unwrap();
    lightr_index::hydrate(&rootfs_dir, &store, ALPINE_REF).unwrap();
    let cwd = PathBuf::from("/");
    let cmd_s = "ip link set eth1 up; \
                 ip link show eth1; \
                 echo '--- sending raw frames on eth1 ---'; \
                 udhcpc -i eth1 -n -q -f -t 4 2>&1 | head -20; \
                 echo '--- arping broadcast ---'; \
                 (arping -I eth1 -c 2 10.69.243.1 2>&1 | head -5) || true; \
                 echo DIAG_DONE";
    let cmd: Vec<String> = vec!["/bin/sh".into(), "-c".into(), cmd_s.into()];
    let engine = engine_for(EngineKind::Vz).unwrap();
    let spec = ExecSpec {
        cwd: &cwd,
        command: &cmd,
        rootfs: Some(&rootfs_dir),
        limits: ResourceLimits::default(),
        net: true,
        net_fd: Some(guest_fd),
        // WP-#102: stale spike (not a workspace member); kept for field-completeness.
        exec_ready_fd: None,
        // WP-#106: stale spike (not a workspace member); kept for field-completeness.
        apparmor: None,
    };
    let code = engine.run(&spec).unwrap();
    let so = std::fs::read_to_string(rootfs_dir.join(".lightr-stdout")).unwrap_or_default();
    eprintln!("  guest exit={code}\n  ---- guest stdout ----");
    for l in so.lines() {
        eprintln!("    | {l}");
    }

    // Give the reader a beat to drain anything still queued, then stop.
    thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::SeqCst);
    let _ = reader.join();

    let total = count.load(Ordering::SeqCst);
    eprintln!("\nDIAG-FRAMES VERDICT: host end received {total} datagram(s) from the guest eth1.");
    if total == 0 {
        eprintln!(
            "  => TRANSPORT BUG: the guest's eth1 TX never reached the host socketpair end.\n  \
             Suspect the engine net_fd path / vz.swift VZFileHandleNetworkDeviceAttachment\n  \
             / fd lifetime — NOT vswitch/dhcp.rs."
        );
    } else {
        eprintln!(
            "  => TRANSPORT OK ({total} frames crossed). The DISCOVER reaches the host;\n  \
             the missing OFFER is therefore in vswitch/dhcp.rs (parse/build) or the\n  \
             switch reply path — compare the dumped DISCOVER bytes vs dhcp::handle."
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

fn classify(f: &[u8]) -> String {
    if f.len() < 14 {
        return format!("(runt {} bytes)", f.len());
    }
    let dst = &f[0..6];
    let src = &f[6..12];
    let et = u16::from_be_bytes([f[12], f[13]]);
    let etn = match et {
        0x0800 => "IPv4",
        0x0806 => "ARP",
        0x86dd => "IPv6",
        _ => "?",
    };
    let bcast = dst.iter().all(|&b| b == 0xff);
    let mut extra = String::new();
    if et == 0x0800 && f.len() >= 14 + 20 + 8 {
        let ihl = (f[14] & 0x0f) as usize * 4;
        let proto = f.get(14 + 9).copied().unwrap_or(0);
        if proto == 17 {
            let uoff = 14 + ihl;
            if f.len() >= uoff + 8 {
                let sp = u16::from_be_bytes([f[uoff], f[uoff + 1]]);
                let dp = u16::from_be_bytes([f[uoff + 2], f[uoff + 3]]);
                extra = format!(" UDP {sp}->{dp}");
                if sp == 68 && dp == 67 {
                    extra.push_str(" [DHCP DISCOVER/REQUEST]");
                }
            }
        }
    }
    format!(
        "dst={} src={} eth=0x{et:04x}({etn}){}{}",
        mac(dst),
        mac(src),
        if bcast { " BCAST" } else { "" },
        extra
    )
}

fn is_dhcp_to_server(f: &[u8]) -> bool {
    if f.len() < 14 + 20 + 8 || u16::from_be_bytes([f[12], f[13]]) != 0x0800 {
        return false;
    }
    let ihl = (f[14] & 0x0f) as usize * 4;
    if f.get(14 + 9).copied().unwrap_or(0) != 17 {
        return false;
    }
    let uoff = 14 + ihl;
    f.len() >= uoff + 8 && u16::from_be_bytes([f[uoff + 2], f[uoff + 3]]) == 67
}

fn mac(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
}
