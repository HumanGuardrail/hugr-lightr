//! WP-C9 switch_host tests. Parallel-safe: every test owns a unique tempdir
//! `home` + unique network id (atomic counter + nanos), never mutates process
//! globals (no `set_var`), and never re-execs (the birth path's `current_exe()`
//! re-exec is proven by the `c9-xproc-switch` example, which RUNs as a genuine
//! 3-process PASS). Here we drive `attach`'s CONNECT path against a switch host
//! started IN-PROCESS via `run_switch_host` on a thread, so the same production
//! API (passfd → add_member → route → refcount self-stop) is exercised end to
//! end across threads without a VM.

use super::*;
use crate::network::NetworkRegistry;
use std::sync::atomic::{AtomicU64, Ordering as O};
use std::time::Duration;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A unique (home, id) per test — no shared paths ⇒ no cross-test interference.
/// SHORT base (`/tmp`, short tokens): an `AF_UNIX` path is bounded by `SUN_LEN`
/// (~104 bytes), and `<home>/net/<id>/ctl.sock` must fit. macOS's long
/// `$TMPDIR` (`/var/folders/.../T/`) blows that budget, so we anchor at `/tmp`.
fn fresh() -> (std::path::PathBuf, String) {
    let n = SEQ.fetch_add(1, O::SeqCst);
    let tok = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        % 1_000_000;
    let home = std::path::PathBuf::from(format!("/tmp/c9-{}-{n}-{tok}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    (home, format!("n{n}{tok}"))
}

#[test]
fn meta_roundtrip() {
    let mac = [0x0a, 0x00, 0x00, 0x11, 0x22, 0x33];
    let ip = Ipv4Addr::new(10, 69, 7, 5);
    let enc = encode_meta(mac, ip, "web");
    let (m, i, n) = decode_meta(&enc).expect("decode");
    assert_eq!(m, mac);
    assert_eq!(i, ip);
    assert_eq!(n, "web");
}

#[test]
fn meta_rejects_short_or_truncated() {
    assert!(decode_meta(&[0u8; 5]).is_none());
    // Claims a 200-byte name but carries none → truncated → None (fail closed).
    let mut bad = vec![0u8; 11];
    bad[10] = 200;
    assert!(decode_meta(&bad).is_none());
}

#[test]
fn election_lock_is_exclusive() {
    let (home, id) = fresh();
    std::fs::create_dir_all(net_dir(&home, &id)).unwrap();
    let path = switch_lock_path(&home, &id);
    let held = ElectionLock::acquire(&path).expect("first acquire");
    // A second acquire from another thread must BLOCK until the first drops.
    let path2 = path.clone();
    let h = std::thread::spawn(move || {
        let _g = ElectionLock::acquire(&path2).expect("second acquire");
        std::time::Instant::now()
    });
    std::thread::sleep(Duration::from_millis(200));
    let released_at = std::time::Instant::now();
    drop(held);
    let acquired_at = h.join().unwrap();
    assert!(
        acquired_at >= released_at,
        "second lock acquired before the first was released"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The keystone: a switch host (started via `run_switch_host` on a thread) +
/// TWO members attached via the production `attach` API. Proves, all over the
/// real passfd/add_member/route path:
///   * A→B Ethernet frame forwarding,
///   * embedded DHCP answers a crafted DISCOVER with the registry IP,
///   * embedded DNS resolves the OTHER member's name,
///   * `detach` of both members → refcount 0 → the switch host SELF-stops
///     (its `run_switch_host` thread returns) and removes its ctl.sock.
#[test]
fn attach_forward_dhcp_dns_then_refcount_self_stop() {
    let (home, id) = fresh();

    // Registry + members FIRST (refcount is lifecycle truth), THEN birth the host.
    let reg = NetworkRegistry::create(&home, &id).unwrap();
    let a = reg.join("a", &[], &[]).unwrap();
    let b = reg.join("b", &[], &[]).unwrap();

    // Start the switch host on a thread (NOT a re-exec): the production body.
    let host_home = home.clone();
    let host_id = id.clone();
    let host = std::thread::spawn(move || run_switch_host(&host_home, &host_id));

    // Wait for the host to bind ctl.sock, then attach both members (CONNECT path).
    let ctl = ctl_sock_path(&home, &id);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ctl.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ga = attach(&home, &id, &a).expect("attach a");
    let gb = attach(&home, &id, &b).expect("attach b");
    let ga = UnixDatagram::from(ga);
    let gb = UnixDatagram::from(gb);
    ga.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    gb.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    // Give the accept loop a beat to add both members.
    std::thread::sleep(Duration::from_millis(200));

    // PROOF 1: A→B Ethernet frame forwarding. Flood first so the switch learns A.
    let payload = b"C9-FRAME-AB";
    let frame = build_eth(b.mac.0, a.mac.0, 0x88b5, payload);
    ga.send(&frame).unwrap();
    let mut buf = vec![0u8; 64 * 1024];
    let n = gb.recv(&mut buf).expect("B receives A's frame");
    assert_eq!(&buf[..n], &frame[..], "B got a different frame than A sent");

    // PROOF 2: DHCP DISCOVER → OFFER carrying A's registry IP.
    let disc = build_dhcp_discover(a.mac.0);
    ga.send(&disc).unwrap();
    let n = ga.recv(&mut buf).expect("DHCP OFFER");
    assert_eq!(
        decode_dhcp_yiaddr(&buf[..n]),
        Some(a.ip),
        "DHCP IP mismatch"
    );

    // PROOF 3: DNS A-query for "b" → B's registry IP (curl-by-name).
    let q = build_dns_query(a.mac.0, a.ip, "b");
    ga.send(&q).unwrap();
    let n = ga.recv(&mut buf).expect("DNS answer");
    assert_eq!(
        decode_dns_first_a(&buf[..n]),
        Some(b.ip),
        "DNS resolve mismatch"
    );

    // PROOF 4: detach both → refcount 0 → host self-stops + reclaims ctl.sock.
    drop(ga);
    drop(gb);
    detach(&home, &id, "a").unwrap();
    let remaining = {
        // detach(b) leaves 0; assert via the registry directly.
        detach(&home, &id, "b").unwrap();
        reg.members().unwrap().len()
    };
    assert_eq!(remaining, 0, "registry not empty after both left");

    // The host's refcount self-watch must observe 0 and return cleanly.
    let joined = host.join().expect("host thread panicked");
    joined.expect("run_switch_host returned an error");
    assert!(!ctl.exists(), "ctl.sock leaked after self-stop");

    let _ = std::fs::remove_dir_all(&home);
}

/// `detach` is idempotent and never errors on an absent member / empty network.
#[test]
fn detach_absent_is_ok() {
    let (home, id) = fresh();
    NetworkRegistry::create(&home, &id).unwrap();
    detach(&home, &id, "ghost").expect("detach of absent member is Ok");
    let _ = std::fs::remove_dir_all(&home);
}

// ── crafted client frames the switch's PROVEN handlers answer (mirror s6) ─────

fn build_eth(dst: [u8; 6], src: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut e = Vec::with_capacity(14 + payload.len());
    e.extend_from_slice(&dst);
    e.extend_from_slice(&src);
    e.extend_from_slice(&ethertype.to_be_bytes());
    e.extend_from_slice(payload);
    e
}

fn ipv4_checksum(h: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0;
    while i + 1 < h.len() {
        sum += u16::from_be_bytes([h[i], h[i + 1]]) as u32;
        i += 2;
    }
    if i < h.len() {
        sum += (h[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[allow(clippy::too_many_arguments)]
fn build_udp_ipv4_eth(
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let mut udp = Vec::with_capacity(udp_len);
    udp.extend_from_slice(&src_port.to_be_bytes());
    udp.extend_from_slice(&dst_port.to_be_bytes());
    udp.extend_from_slice(&(udp_len as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(payload);

    let total = 20 + udp.len();
    let mut ip = Vec::with_capacity(total);
    ip.push((4 << 4) | 5);
    ip.push(0);
    ip.extend_from_slice(&(total as u16).to_be_bytes());
    ip.extend_from_slice(&[0, 0]);
    ip.extend_from_slice(&[0x40, 0x00]);
    ip.push(64);
    ip.push(17);
    ip.extend_from_slice(&[0, 0]);
    ip.extend_from_slice(&src_ip.octets());
    ip.extend_from_slice(&dst_ip.octets());
    let csum = ipv4_checksum(&ip);
    ip[10..12].copy_from_slice(&csum.to_be_bytes());
    ip.extend_from_slice(&udp);

    build_eth(dst_mac, src_mac, 0x0800, &ip)
}

fn build_dhcp_discover(mac: [u8; 6]) -> Vec<u8> {
    const BOOTP_FIXED_LEN: usize = 236;
    let mut bootp = vec![0u8; BOOTP_FIXED_LEN];
    bootp[0] = 1;
    bootp[1] = 1;
    bootp[2] = 6;
    bootp[4..8].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    bootp[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
    bootp[28..34].copy_from_slice(&mac);
    bootp.extend_from_slice(&0x6382_5363u32.to_be_bytes());
    bootp.push(53);
    bootp.push(1);
    bootp.push(1);
    bootp.push(255);
    build_udp_ipv4_eth(
        mac,
        [0xff; 6],
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::new(255, 255, 255, 255),
        68,
        67,
        &bootp,
    )
}

fn decode_dhcp_yiaddr(frame: &[u8]) -> Option<Ipv4Addr> {
    let ip = frame.get(14..)?;
    if ip.first()? >> 4 != 4 {
        return None;
    }
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    let udp = ip.get(ihl..)?;
    if udp.len() < 8 || u16::from_be_bytes([udp[0], udp[1]]) != 67 {
        return None;
    }
    let bootp = udp.get(8..)?;
    if bootp.first()? != &2 {
        return None;
    }
    let y = bootp.get(16..20)?;
    Some(Ipv4Addr::new(y[0], y[1], y[2], y[3]))
}

fn build_dns_query(mac: [u8; 6], src_ip: Ipv4Addr, name: &str) -> Vec<u8> {
    let mut dns = Vec::new();
    dns.extend_from_slice(&0x1234u16.to_be_bytes());
    dns.extend_from_slice(&0x0100u16.to_be_bytes());
    dns.extend_from_slice(&1u16.to_be_bytes());
    dns.extend_from_slice(&0u16.to_be_bytes());
    dns.extend_from_slice(&0u16.to_be_bytes());
    dns.extend_from_slice(&0u16.to_be_bytes());
    for label in name.split('.') {
        dns.push(label.len() as u8);
        dns.extend_from_slice(label.as_bytes());
    }
    dns.push(0);
    dns.extend_from_slice(&1u16.to_be_bytes());
    dns.extend_from_slice(&1u16.to_be_bytes());
    let gw = Ipv4Addr::new(
        src_ip.octets()[0],
        src_ip.octets()[1],
        src_ip.octets()[2],
        1,
    );
    build_udp_ipv4_eth(mac, [0x02, 0, 0, 0, 0, 1], src_ip, gw, 0xC001, 53, &dns)
}

fn decode_dns_first_a(frame: &[u8]) -> Option<Ipv4Addr> {
    let ip = frame.get(14..)?;
    if ip.first()? >> 4 != 4 {
        return None;
    }
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    let udp = ip.get(ihl..)?;
    if udp.len() < 8 || u16::from_be_bytes([udp[0], udp[1]]) != 53 {
        return None;
    }
    let dns = udp.get(8..)?;
    if dns.len() < 12 {
        return None;
    }
    if u16::from_be_bytes([dns[6], dns[7]]) < 1 {
        return None;
    }
    let mut pos = 12;
    loop {
        let len = *dns.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 != 0 {
            pos += 2;
            break;
        }
        pos += 1 + len;
    }
    pos += 4;
    let name0 = *dns.get(pos)?;
    if name0 & 0xc0 == 0xc0 {
        pos += 2;
    } else {
        loop {
            let len = *dns.get(pos)? as usize;
            pos += 1;
            if len == 0 {
                break;
            }
            pos += len;
        }
    }
    let rtype = u16::from_be_bytes([*dns.get(pos)?, *dns.get(pos + 1)?]);
    pos += 8; // TYPE(2)+CLASS(2)+TTL(4)
    let rdlen = u16::from_be_bytes([*dns.get(pos)?, *dns.get(pos + 1)?]) as usize;
    pos += 2;
    if rtype != 1 || rdlen != 4 {
        return None;
    }
    let rd = dns.get(pos..pos + 4)?;
    Some(Ipv4Addr::new(rd[0], rd[1], rd[2], rd[3]))
}
