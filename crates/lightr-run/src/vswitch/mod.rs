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
pub mod passfd;
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

use switch::{arp_gateway_reply, forward, ForwardDecision, MacTable, PortId};

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

    // 2.5 ARP for the embedded gateway → synthesize a reply on the ingress port.
    //     The gateway (DHCP router + DNS server) has no member port, so nothing
    //     else answers its ARP; without this a guest can never resolve the
    //     nameserver's MAC and DNS-by-name silently fails.
    if let Some(reply) = arp_gateway_reply(frame, shared.gateway, dhcp::GATEWAY_MAC) {
        return vec![(my_port, reply)];
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
#[path = "mod_tests.rs"]
mod tests;
