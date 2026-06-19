//! Daemonless, network-scoped userspace L2 switch (ADR-0018, F-304 Phase-2).
//!
//! Owns one `AF_UNIX`/`SOCK_DGRAM` socket per member (the host end of each
//! guest's `VZFileHandleNetworkDeviceAttachment` — de-risk spike S5-FHNET
//! proved one datagram == one Ethernet frame). A poll/forward thread runs the
//! [`switch`] (MAC learning + flood), and intercepts DHCP/DNS frames destined
//! for the gateway, answering via [`dhcp`] / [`dns`]. The switch is born by the
//! first member's supervisor and stops when the last member leaves (the
//! registry refcount arbitrates) — nothing of ours is resident between runs.
//!
//! ## Threading model
//!
//! Thread-per-member, matching the codebase's thread-per-conn std-net style
//! (see [`crate::portforward`]); no async runtime. Each member owns a receive
//! thread that blocks on `recv` with a 200 ms read timeout so it can observe
//! the shared stop flag. On each datagram (== exactly one Ethernet frame) the
//! thread calls the pure [`route`] helper, which decides — under per-frame
//! locks — DHCP/DNS interception then L2 forward, and returns the list of
//! `(PortId, frame)` sends. The thread performs the actual socket I/O (sending
//! to peer ports) *outside* every lock. Factoring routing into [`route`] keeps
//! the forwarding logic deterministically unit-testable (no threads/VM); the
//! socket path gets a focused smoke test.
//!
//! ## Buffer sizing (spike S5-FHNET finding)
//!
//! Reads use a [`RECV_BUF_LEN`]-byte (64 KiB) buffer because VZ can hand
//! `>1514 B` GSO aggregates as a single datagram; a 1514-byte buffer would
//! truncate them. We also best-effort bump `SO_RCVBUF` on each member socket
//! (skip silently if the `setsockopt` fails).

pub mod dhcp;
pub mod dns;
pub mod switch;

use crate::network::{NetworkId, Subnet};
use std::io;
use std::net::Ipv4Addr;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use switch::{forward, ForwardDecision, MacTable, PortId};

/// Read timeout on each member socket: short enough that a member thread
/// re-checks the stop flag promptly, long enough to avoid a busy spin.
const RECV_TIMEOUT: Duration = Duration::from_millis(200);

/// Receive buffer: ≥64 KiB because VZ can hand a single datagram carrying a
/// `>1514 B` GSO aggregate (spike S5-FHNET). Sizing below this truncates.
const RECV_BUF_LEN: usize = 64 * 1024;

/// Target `SO_RCVBUF` we *try* to set per member socket (best-effort; the
/// kernel may clamp, and a failure is ignored — the 64 KiB userspace buffer is
/// what actually bounds a single `recv`).
const TARGET_RCVBUF: libc::c_int = 1 << 20; // 1 MiB

/// A switch port: a member's cloned send socket + its identity, so any member
/// thread can deliver a unicast/flood frame to the right peer. The owning
/// member's *receive* socket lives in its thread; this is the send side.
struct PortSocket {
    /// `Some` while the member is attached; `None` after `remove_member` so a
    /// stale [`PortId`] in the [`MacTable`] simply delivers nowhere.
    sock: Option<UnixDatagram>,
    /// Member name, for `remove_member` lookup.
    name: String,
}

/// Shared, lock-guarded switch state. One instance lives behind an [`Arc`] and
/// is cloned into every member thread. Locks are taken per-frame and released
/// before any socket I/O.
struct Shared {
    /// MAC-learning table (src MAC → ingress port).
    macs: Mutex<MacTable>,
    /// DHCP lease store, pre-seeded with each member's registry-assigned IP.
    leases: Mutex<dhcp::LeaseStore>,
    /// DNS name table, pre-seeded with each member's name → IP.
    names: Mutex<dns::NameTable>,
    /// Port → send socket registry (indexed by [`PortId`]). Appended under lock
    /// in `add_member`; a removed member's slot has its `sock` taken.
    ports: Mutex<Vec<PortSocket>>,
    /// This network's subnet (gateway = DHCP server-id/router, also DNS IP).
    subnet: Subnet,
    /// The switch's virtual gateway IP (== `subnet.gateway`); DHCP/DNS source.
    gateway: Ipv4Addr,
    /// Host upstream resolver (first nameserver in `/etc/resolv.conf`), or
    /// `None` — DNS misses then stay transparent (see [`dns`] not-found policy).
    upstream: Option<Ipv4Addr>,
}

/// A running switch instance for one network.
pub struct VSwitch {
    /// All shared, lock-guarded state (cloned into member threads).
    shared: Arc<Shared>,
    /// Set on `shutdown`/`Drop`; member threads observe it and exit.
    stop: Arc<AtomicBool>,
    /// One receive thread per member, in join order. Behind a `Mutex` because
    /// the frozen `add_member` takes `&self` yet must record its spawned thread;
    /// `shutdown`/`Drop` drain + join.
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// `NetworkId` this switch serves (diagnostics; the registry refcount is the
    /// real lifecycle authority).
    id: NetworkId,
}

impl VSwitch {
    /// Start a switch for `id` on `subnet` (no members yet). Members — and their
    /// threads — are added by [`add_member`]. The registry refcount makes this
    /// effectively once-per-network.
    ///
    /// [`add_member`]: VSwitch::add_member
    pub fn start(id: &NetworkId, subnet: Subnet) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            macs: Mutex::new(MacTable::new()),
            leases: Mutex::new(dhcp::LeaseStore::new()),
            names: Mutex::new(dns::NameTable::new()),
            ports: Mutex::new(Vec::new()),
            subnet,
            gateway: subnet.gateway,
            upstream: host_upstream_dns(),
        });
        Ok(VSwitch {
            shared,
            stop: Arc::new(AtomicBool::new(false)),
            threads: Mutex::new(Vec::new()),
            id: id.clone(),
        })
    }

    /// Add a member: take ownership of the host end (`host_fd`) of its
    /// socketpair, register it with its assigned MAC/IP/name for switching +
    /// DHCP + DNS, and spawn its receive thread.
    pub fn add_member(
        &self,
        host_fd: RawFd,
        mac: [u8; 6],
        ip: Ipv4Addr,
        name: &str,
    ) -> std::io::Result<()> {
        // Wrap the host end of the socketpair. SAFETY: the caller transfers
        // ownership of `host_fd` (the host end of a VZ file-handle attachment);
        // from here the `UnixDatagram` owns and will close it.
        let recv_sock = unsafe { UnixDatagram::from_raw_fd(host_fd) };
        recv_sock.set_read_timeout(Some(RECV_TIMEOUT))?;
        // Best-effort: enlarge the kernel receive buffer for GSO bursts.
        bump_rcvbuf(&recv_sock);

        // A clone for the shared send registry (peers deliver frames to it).
        let send_sock = recv_sock.try_clone()?;

        // Assign the next PortId = the index this member takes in `ports`, and
        // seed the lease/name tables, all under their respective locks.
        let port: PortId = {
            let mut ports = self.shared.ports.lock().unwrap();
            let port = ports.len();
            ports.push(PortSocket {
                sock: Some(send_sock),
                name: name.to_string(),
            });
            port
        };
        self.shared.leases.lock().unwrap().insert(mac, ip);
        self.shared
            .names
            .lock()
            .unwrap()
            .insert(name.to_ascii_lowercase(), ip);

        // Spawn the member's receive thread.
        let shared = Arc::clone(&self.shared);
        let stop = Arc::clone(&self.stop);
        let handle = std::thread::Builder::new()
            .name(format!("vswitch-{}-{}", self.id, name))
            .spawn(move || member_loop(recv_sock, port, &shared, &stop))?;

        // Record the handle so `shutdown`/`Drop` can join it.
        self.threads.lock().unwrap().push(handle);
        Ok(())
    }

    /// Remove a member by name: drop its send socket (so peers can no longer
    /// reach it) and signal its receive thread, which exits on the next timeout.
    /// The guest sees its carrier drop when the host end closes.
    pub fn remove_member(&self, name: &str) -> std::io::Result<()> {
        let mut ports = self.shared.ports.lock().unwrap();
        for p in ports.iter_mut() {
            if p.name == name {
                // Dropping the cloned send socket closes that descriptor; the
                // member's own receive socket closes when its thread ends. The
                // thread ends because we cannot signal one thread individually
                // without a per-member flag, so we close the send side now and
                // let the receive thread fall out on its next recv error/EOF or
                // at global shutdown. Marking the slot empty also makes any
                // stale MacTable entry deliver nowhere.
                p.sock = None;
            }
        }
        Ok(())
    }

    /// Stop the switch: signal all member threads and join them.
    pub fn shutdown(self) -> std::io::Result<()> {
        self.teardown();
        Ok(())
    }

    /// Signal stop, drop every send socket so no thread blocks on a peer, then
    /// drain + join the member threads. Idempotent — `shutdown` and `Drop` both
    /// call it; the second pass finds the flag set and the handle vec empty.
    fn teardown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut ports) = self.shared.ports.lock() {
            for p in ports.iter_mut() {
                p.sock = None;
            }
        }
        let handles: Vec<JoinHandle<()>> = match self.threads.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(_) => return,
        };
        for h in handles {
            let _ = h.join();
        }
    }
}

impl Drop for VSwitch {
    fn drop(&mut self) {
        // Best-effort teardown if the caller dropped without `shutdown`.
        self.teardown();
    }
}

// ── per-member receive loop ──────────────────────────────────────────────────

/// Block on `sock`, route each received frame, and perform the resulting sends.
/// Exits when the stop flag is set or the socket hard-errors.
fn member_loop(sock: UnixDatagram, my_port: PortId, shared: &Arc<Shared>, stop: &AtomicBool) {
    let mut buf = vec![0u8; RECV_BUF_LEN];
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let n = match sock.recv(&mut buf) {
            Ok(0) => continue, // empty datagram; ignore
            Ok(n) => n,
            Err(e) => match e.kind() {
                // Read timeout → loop to re-check the stop flag.
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => continue,
                io::ErrorKind::Interrupted => continue,
                // Hard socket error (peer closed, fd invalid) → exit the thread.
                _ => return,
            },
        };
        let frame = &buf[..n];
        let sends = route(frame, my_port, shared);
        deliver(shared, &sends);
    }
}

/// Perform the `(PortId, frame)` sends produced by [`route`], skipping any port
/// whose socket has been removed. All socket I/O happens here, outside locks
/// except the short clone of each target socket.
fn deliver(shared: &Arc<Shared>, sends: &[(PortId, Vec<u8>)]) {
    for (port, frame) in sends {
        // Clone the target socket under the lock, then send unlocked.
        let target = {
            let ports = shared.ports.lock().unwrap();
            ports
                .get(*port)
                .and_then(|p| p.sock.as_ref())
                .and_then(|s| s.try_clone().ok())
        };
        if let Some(s) = target {
            let _ = s.send(frame);
        }
    }
}

// ── the pure routing core (deterministically testable) ───────────────────────

/// Decide what to do with one ingress `frame` arriving on `my_port`, returning
/// the list of `(destination port, frame bytes)` to send. This is the switch's
/// per-frame dispatch, factored out of the socket loop so it is unit-testable
/// without threads or a VM.
///
/// Dispatch order (each step holds its lock only for that step):
/// 1. **DHCP** — if [`dhcp::handle`] yields a reply, send it back on `my_port`.
/// 2. **DNS** — else if [`dns::handle`] yields a reply, send it back on `my_port`.
/// 3. **L2 forward** — else [`switch::forward`]: `Unicast(p)` → `[(p, frame)]`;
///    `Flood` → every other attached port; `Drop` → nothing.
fn route(frame: &[u8], my_port: PortId, shared: &Shared) -> Vec<(PortId, Vec<u8>)> {
    // 1. DHCP interception (server port 67) → reply on the ingress port.
    {
        let mut leases = shared.leases.lock().unwrap();
        if let Some(reply) = dhcp::handle(frame, &mut leases, &shared.subnet, shared.gateway) {
            return vec![(my_port, reply)];
        }
    }

    // 2. DNS interception (server port 53) → reply on the ingress port.
    {
        let names = shared.names.lock().unwrap();
        if let Some(reply) = dns::handle(frame, &names, shared.upstream) {
            return vec![(my_port, reply)];
        }
    }

    // 3. L2 learning switch.
    let decision = {
        let mut macs = shared.macs.lock().unwrap();
        forward(frame, my_port, &mut macs)
    };
    match decision {
        ForwardDecision::Unicast(port) => vec![(port, frame.to_vec())],
        ForwardDecision::Flood => {
            // Flood to every attached port except the ingress.
            let ports = shared.ports.lock().unwrap();
            (0..ports.len())
                .filter(|&p| p != my_port && ports[p].sock.is_some())
                .map(|p| (p, frame.to_vec()))
                .collect()
        }
        ForwardDecision::Drop => Vec::new(),
    }
}

// ── host upstream resolver discovery ─────────────────────────────────────────

/// Read the first `nameserver` from the host `/etc/resolv.conf`, used as the
/// DNS upstream for names not in the network's table. Returns `None` if the
/// file is absent/unreadable or carries no IPv4 nameserver — DNS then stays
/// transparent on a miss (see [`dns`] not-found policy).
fn host_upstream_dns() -> Option<Ipv4Addr> {
    let text = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("nameserver") {
            if let Some(addr) = rest.split_whitespace().next() {
                if let Ok(ip) = addr.parse::<Ipv4Addr>() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

// ── socket option helper ─────────────────────────────────────────────────────

/// Best-effort enlarge the kernel receive buffer (`SO_RCVBUF`) on `sock` to
/// absorb GSO bursts. Any failure is ignored — the userspace [`RECV_BUF_LEN`]
/// buffer is the hard bound on a single datagram.
fn bump_rcvbuf(sock: &UnixDatagram) {
    use std::os::unix::io::AsRawFd;
    let fd = sock.as_raw_fd();
    let val = TARGET_RCVBUF;
    // SAFETY: `fd` is a valid socket fd owned by `sock`; `&val` points to a
    // single `c_int` matching the documented length for `SO_RCVBUF`.
    unsafe {
        let _ = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &val as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
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
}
