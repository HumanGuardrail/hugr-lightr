use super::*;

fn test_subnet() -> Subnet {
    Subnet {
        base: Ipv4Addr::new(10, 69, 0, 0),
        prefix: 24,
        gateway: Ipv4Addr::new(10, 69, 0, 1),
    }
}

/// A `Shared` with two attached ports backed by the host ends of two
/// socketpairs; returns the shared state plus the guest ends to drive.
fn shared_with_two_ports() -> (Arc<Shared>, [UnixDatagram; 2]) {
    let (host0, guest0) = UnixDatagram::pair().unwrap();
    let (host1, guest1) = UnixDatagram::pair().unwrap();
    let shared = Arc::new(Shared {
        macs: Mutex::new(MacTable::new()),
        leases: Mutex::new(dhcp::LeaseStore::new()),
        names: Mutex::new(dns::NameTable::new()),
        ports: Mutex::new(vec![
            PortSocket {
                sock: Some(host0),
                name: "a".into(),
            },
            PortSocket {
                sock: Some(host1),
                name: "b".into(),
            },
        ]),
        subnet: test_subnet(),
        gateway: test_subnet().gateway,
        upstream: None,
    });
    (shared, [guest0, guest1])
}

// ── L2 forwarding (route helper, deterministic) ─────────────────────────

const MAC_A: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
const MAC_B: [u8; 6] = [0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
const BCAST: [u8; 6] = [0xff; 6];

/// Minimal Ethernet frame (dst, src, ethertype 0x0800, no payload kept short
/// but ≥14 bytes so the switch accepts it; DHCP/DNS parsers reject it).
fn eth(dst: [u8; 6], src: [u8; 6]) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&[0x08, 0x00]);
    // A few payload bytes so it is a plausible (if tiny) frame.
    f.extend_from_slice(&[0u8; 20]);
    f
}

#[test]
fn route_floods_broadcast_to_other_ports_only() {
    let (shared, _guests) = shared_with_two_ports();
    // Broadcast from port 0 → floods to port 1 only (not the ingress).
    let sends = route(&eth(BCAST, MAC_A), 0, &shared);
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].0, 1, "flood goes to the other port, not ingress");
}

#[test]
fn route_learns_then_unicasts() {
    let (shared, _guests) = shared_with_two_ports();
    // Port 0 sends a broadcast with src=MAC_A → switch learns A on port 0.
    let _ = route(&eth(BCAST, MAC_A), 0, &shared);
    // Port 1 sends a frame to MAC_A → must unicast to port 0 exactly.
    let sends = route(&eth(MAC_A, MAC_B), 1, &shared);
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].0, 0, "known unicast resolves to the learned port");
}

#[test]
fn route_drops_short_frame() {
    let (shared, _guests) = shared_with_two_ports();
    let sends = route(&[0u8; 8], 0, &shared);
    assert!(sends.is_empty(), "sub-14-byte frame is dropped");
}

#[test]
fn route_does_not_flood_to_removed_port() {
    let (shared, _guests) = shared_with_two_ports();
    // Remove port 1's send socket.
    shared.ports.lock().unwrap()[1].sock = None;
    let sends = route(&eth(BCAST, MAC_A), 0, &shared);
    assert!(sends.is_empty(), "flood skips a removed port");
}

// ── DHCP interception (route helper) ────────────────────────────────────

/// Build a DHCP DISCOVER frame from `client_mac` (broadcast flag set), the
/// same wire shape `udhcpc`/`dhclient` emit. Mirrors the dhcp module's own
/// test builder so we exercise the real `dhcp::handle` via `route`.
fn dhcp_discover(client_mac: [u8; 6]) -> Vec<u8> {
    const MAGIC: u32 = 0x6382_5363;
    // BOOTP fixed header (236 bytes).
    let mut bootp = vec![0u8; 236];
    bootp[0] = 1; // BOOTREQUEST
    bootp[1] = 1; // htype ethernet
    bootp[2] = 6; // hlen
    bootp[4..8].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // xid
    bootp[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast flag
    bootp[28..34].copy_from_slice(&client_mac); // chaddr
    bootp.extend_from_slice(&MAGIC.to_be_bytes());
    bootp.extend_from_slice(&[53, 1, 1]); // option 53 = DISCOVER
    bootp.push(255); // END

    // UDP (68 → 67), checksum 0.
    let mut udp = Vec::new();
    udp.extend_from_slice(&68u16.to_be_bytes());
    udp.extend_from_slice(&67u16.to_be_bytes());
    udp.extend_from_slice(&((8 + bootp.len()) as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(&bootp);

    // IPv4 (0.0.0.0 → 255.255.255.255), proto UDP, checksum left 0 (the
    // switch's dhcp parser does not verify the IP checksum on ingress).
    let total = 20 + udp.len();
    let mut ip = Vec::new();
    ip.push(0x45);
    ip.push(0);
    ip.extend_from_slice(&(total as u16).to_be_bytes());
    ip.extend_from_slice(&[0, 0, 0x40, 0x00]);
    ip.push(64);
    ip.push(17); // UDP
    ip.extend_from_slice(&[0, 0]); // checksum 0
    ip.extend_from_slice(&Ipv4Addr::UNSPECIFIED.octets());
    ip.extend_from_slice(&Ipv4Addr::BROADCAST.octets());
    ip.extend_from_slice(&udp);

    // Ethernet (broadcast dst, client src, IPv4).
    let mut eth = Vec::new();
    eth.extend_from_slice(&BCAST);
    eth.extend_from_slice(&client_mac);
    eth.extend_from_slice(&0x0800u16.to_be_bytes());
    eth.extend_from_slice(&ip);
    eth
}

#[test]
fn route_dhcp_discover_yields_offer_on_ingress_port() {
    let (shared, _guests) = shared_with_two_ports();
    let client_mac = [0x52, 0x54, 0x00, 0x01, 0x02, 0x03];
    // Seed the lease the registry would have assigned this MAC.
    shared
        .leases
        .lock()
        .unwrap()
        .insert(client_mac, Ipv4Addr::new(10, 69, 0, 42));

    let sends = route(&dhcp_discover(client_mac), 0, &shared);
    assert_eq!(sends.len(), 1, "DHCP reply is a single send");
    assert_eq!(sends[0].0, 0, "DHCP reply goes back on the ingress port");
    // The reply must be a DHCP OFFER (option 53 == 2). Locate it past the
    // magic cookie at eth(14)+ip(20)+udp(8)+bootp(236)+cookie(4).
    let reply = &sends[0].1;
    let opt_start = 14 + 20 + 8 + 236 + 4;
    assert_eq!(reply[opt_start], 53, "first option is message-type");
    assert_eq!(reply[opt_start + 2], 2, "message type == OFFER (2)");
}

// ── socket-level smoke test (thread + UnixDatagram round-trip) ──────────

#[test]
fn socket_smoke_dhcp_offer_round_trips() {
    // Drive the full thread path once: a real DISCOVER in on the guest end,
    // an OFFER frame back on the same guest end.
    let id = "smoke-net".to_string();
    let sw = VSwitch::start(&id, test_subnet()).unwrap();

    let (host, guest) = UnixDatagram::pair().unwrap();
    guest
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let client_mac = [0x52, 0x54, 0x00, 0x0a, 0x0b, 0x0c];
    let ip = Ipv4Addr::new(10, 69, 0, 50);

    // Hand the host end to the switch (transfers fd ownership).
    use std::os::unix::io::IntoRawFd;
    sw.add_member(host.into_raw_fd(), client_mac, ip, "smoke")
        .unwrap();

    // Send a DISCOVER from the guest end.
    guest.send(&dhcp_discover(client_mac)).unwrap();

    // Expect an OFFER frame back, within the recv timeout.
    let mut buf = vec![0u8; RECV_BUF_LEN];
    let n = guest.recv(&mut buf).expect("offer frame must arrive");
    assert!(n > 240, "reply is a full DHCP frame");
    let opt_start = 14 + 20 + 8 + 236 + 4;
    assert_eq!(buf[opt_start], 53);
    assert_eq!(buf[opt_start + 2], 2, "OFFER");

    sw.shutdown().unwrap();
}

#[test]
fn socket_smoke_unicast_between_two_members() {
    // A,B attached. A broadcasts (learns A), then B sends a frame to A's MAC
    // → it must arrive on A's guest end.
    let id = "smoke-switch".to_string();
    let sw = VSwitch::start(&id, test_subnet()).unwrap();

    let (host_a, guest_a) = UnixDatagram::pair().unwrap();
    let (host_b, guest_b) = UnixDatagram::pair().unwrap();
    guest_a
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    guest_b
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();

    use std::os::unix::io::IntoRawFd;
    sw.add_member(
        host_a.into_raw_fd(),
        MAC_A,
        Ipv4Addr::new(10, 69, 0, 2),
        "a",
    )
    .unwrap();
    sw.add_member(
        host_b.into_raw_fd(),
        MAC_B,
        Ipv4Addr::new(10, 69, 0, 3),
        "b",
    )
    .unwrap();

    // A → broadcast: the switch learns MAC_A is on port 0. (B's guest end
    // receives the flood; drain it so it does not confuse the next recv.)
    guest_a.send(&eth(BCAST, MAC_A)).unwrap();
    let mut drain = vec![0u8; RECV_BUF_LEN];
    let _ = guest_b.recv(&mut drain); // flood copy to B

    // B → unicast to MAC_A: must land on A's guest end.
    guest_b.send(&eth(MAC_A, MAC_B)).unwrap();
    let mut buf = vec![0u8; RECV_BUF_LEN];
    let n = guest_a.recv(&mut buf).expect("unicast must reach A");
    assert!(n >= 14);
    assert_eq!(&buf[0..6], &MAC_A, "frame dst is MAC_A");
    assert_eq!(&buf[6..12], &MAC_B, "frame src is MAC_B");

    sw.shutdown().unwrap();
}

#[test]
fn remove_member_marks_port_unreachable() {
    let id = "rm-net".to_string();
    let sw = VSwitch::start(&id, test_subnet()).unwrap();
    let (host, _guest) = UnixDatagram::pair().unwrap();
    use std::os::unix::io::IntoRawFd;
    sw.add_member(
        host.into_raw_fd(),
        MAC_A,
        Ipv4Addr::new(10, 69, 0, 9),
        "gone",
    )
    .unwrap();
    sw.remove_member("gone").unwrap();
    // The port's send socket is now taken.
    assert!(sw.shared.ports.lock().unwrap()[0].sock.is_none());
    sw.shutdown().unwrap();
}
