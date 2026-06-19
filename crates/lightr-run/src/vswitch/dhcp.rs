//! Minimal embedded DHCP server (ADR-0018, F-304 Phase-2).
//!
//! Answers DISCOVER/REQUEST with OFFER/ACK from the network's subnet, leasing
//! the deterministic IP the registry already assigned each MAC, and advertising
//! the gateway + DNS (both = the switch's virtual IP). Frame-in / frame-out so
//! the [`super::VSwitch`] can splice the reply straight back onto the wire.
//! Pure parse/build — unit-testable with captured client packets (no VM).
//!
//! CONTRACT STUB (ADR-0018, WP-C3): signatures frozen; WP-C3 fills the bodies,
//! adds unit tests, and REMOVES the `#![allow]`.

use crate::network::Subnet;
use std::collections::HashMap;
use std::net::Ipv4Addr;

// ---------------------------------------------------------------------------
// Wire-format constants
// ---------------------------------------------------------------------------

const ETH_HDR_LEN: usize = 14;
const ETHERTYPE_IPV4: u16 = 0x0800;

const IP_PROTO_UDP: u8 = 17;
const IPV4_MIN_IHL_WORDS: u8 = 5;
const IPV4_MIN_HDR_LEN: usize = 20;

const UDP_HDR_LEN: usize = 8;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;

/// BOOTP fixed header length (op..file inclusive), before the options area.
const BOOTP_FIXED_LEN: usize = 236;
/// `0x63825363` — the DHCP magic cookie that precedes the options.
const DHCP_MAGIC_COOKIE: u32 = 0x6382_5363;

const BOOTREQUEST: u8 = 1;
const BOOTREPLY: u8 = 2;
const HTYPE_ETHERNET: u8 = 1;
const HLEN_ETHERNET: u8 = 6;

/// The BOOTP `flags` broadcast bit (RFC 2131 §2): set when the client cannot
/// yet receive unicast IP, so the reply must be broadcast.
const BOOTP_FLAG_BROADCAST: u16 = 0x8000;

// DHCP option codes.
const OPT_PAD: u8 = 0;
const OPT_SUBNET_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MESSAGE_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_END: u8 = 255;

// DHCP message types (option 53 values).
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;

/// Lease time advertised in OFFER/ACK (option 51), in seconds.
const LEASE_SECS: u32 = 3600;

/// Locally-administered, unicast gateway MAC for the switch's virtual port.
/// Bit 0 of the first octet = 0 (unicast); bit 1 = 1 (locally administered).
/// `udhcpc`/`dhclient` only need a stable source MAC for the offer/ack.
const GATEWAY_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

const BROADCAST_MAC: [u8; 6] = [0xff; 6];
const BROADCAST_IP: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);

/// Lease store: client MAC → leased IP. Pre-seeded from the registry's
/// deterministic allocation so DHCP simply hands back the assigned address.
#[derive(Default)]
pub struct LeaseStore {
    leases: HashMap<[u8; 6], Ipv4Addr>,
}

impl LeaseStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed the fixed lease the registry assigned to `mac`.
    pub fn insert(&mut self, mac: [u8; 6], ip: Ipv4Addr) {
        self.leases.insert(mac, ip);
    }

    /// The IP the registry assigned to `mac`, if any.
    fn get(&self, mac: &[u8; 6]) -> Option<Ipv4Addr> {
        self.leases.get(mac).copied()
    }
}

/// Parsed view of an inbound BOOTP/DHCP request we recognized.
struct DhcpRequest {
    /// Client MAC (BOOTP `chaddr`, first 6 bytes).
    chaddr: [u8; 6],
    /// Transaction id (`xid`), echoed verbatim in the reply.
    xid: [u8; 4],
    /// `secs` field, echoed.
    secs: [u8; 2],
    /// Whether the broadcast flag is set (reply must be broadcast).
    broadcast: bool,
    /// `giaddr` (relay) — echoed.
    giaddr: [u8; 4],
    /// DHCP message type from option 53.
    msg_type: u8,
}

/// Handle one inbound DHCP frame (full Ethernet/IP/UDP/BOOTP). Returns the
/// reply FRAME to send back on the ingress port, or `None` if not DHCP.
pub fn handle(
    frame: &[u8],
    leases: &mut LeaseStore,
    subnet: &Subnet,
    dns_ip: Ipv4Addr,
) -> Option<Vec<u8>> {
    let req = parse_dhcp(frame)?;

    // Only DISCOVER/REQUEST are answered; map to the reply type.
    let reply_type = match req.msg_type {
        DHCP_DISCOVER => DHCP_OFFER,
        DHCP_REQUEST => DHCP_ACK,
        _ => return None,
    };

    // The registry owns allocation: without a pre-seeded lease we stay silent.
    let yiaddr = leases.get(&req.chaddr)?;

    Some(build_reply(&req, reply_type, yiaddr, subnet, dns_ip))
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse Ethernet → IPv4 → UDP(68→67) → BOOTP/DHCP. Returns `None` the moment
/// any layer fails to match (fail-closed: a non-DHCP frame is never touched).
fn parse_dhcp(frame: &[u8]) -> Option<DhcpRequest> {
    // --- Ethernet ---
    if frame.len() < ETH_HDR_LEN {
        return None;
    }
    let ethertype = be16(&frame[12..14]);
    if ethertype != ETHERTYPE_IPV4 {
        return None;
    }
    let l3 = &frame[ETH_HDR_LEN..];

    // --- IPv4 ---
    if l3.len() < IPV4_MIN_HDR_LEN {
        return None;
    }
    let version = l3[0] >> 4;
    let ihl_words = l3[0] & 0x0f;
    if version != 4 || ihl_words < IPV4_MIN_IHL_WORDS {
        return None;
    }
    let ip_hdr_len = ihl_words as usize * 4;
    if l3.len() < ip_hdr_len {
        return None;
    }
    if l3[9] != IP_PROTO_UDP {
        return None;
    }
    // total_length bounds the L4 payload (guards against trailing padding).
    let total_len = be16(&l3[2..4]) as usize;
    if total_len < ip_hdr_len || total_len > l3.len() {
        return None;
    }
    let l4 = &l3[ip_hdr_len..total_len];

    // --- UDP ---
    if l4.len() < UDP_HDR_LEN {
        return None;
    }
    let src_port = be16(&l4[0..2]);
    let dst_port = be16(&l4[2..4]);
    if src_port != DHCP_CLIENT_PORT || dst_port != DHCP_SERVER_PORT {
        return None;
    }
    let udp_len = be16(&l4[4..6]) as usize;
    if udp_len < UDP_HDR_LEN || udp_len > l4.len() {
        return None;
    }
    let bootp = &l4[UDP_HDR_LEN..udp_len];

    // --- BOOTP / DHCP ---
    parse_bootp(bootp)
}

/// Parse the BOOTP fixed header + DHCP options. Requires a BOOTREQUEST, an
/// Ethernet hardware type, the magic cookie, and an option-53 message type.
fn parse_bootp(bootp: &[u8]) -> Option<DhcpRequest> {
    // Need the fixed header + magic cookie at minimum.
    if bootp.len() < BOOTP_FIXED_LEN + 4 {
        return None;
    }
    let op = bootp[0];
    let htype = bootp[1];
    let hlen = bootp[2];
    if op != BOOTREQUEST || htype != HTYPE_ETHERNET || hlen != HLEN_ETHERNET {
        return None;
    }

    let mut xid = [0u8; 4];
    xid.copy_from_slice(&bootp[4..8]);
    let mut secs = [0u8; 2];
    secs.copy_from_slice(&bootp[8..10]);
    let flags = be16(&bootp[10..12]);
    let broadcast = flags & BOOTP_FLAG_BROADCAST != 0;

    let mut giaddr = [0u8; 4];
    giaddr.copy_from_slice(&bootp[24..28]);

    // chaddr is 16 bytes at offset 28; the first `hlen` (6) are the MAC.
    let mut chaddr = [0u8; 6];
    chaddr.copy_from_slice(&bootp[28..34]);

    // Magic cookie precedes the options area.
    if be32(&bootp[BOOTP_FIXED_LEN..BOOTP_FIXED_LEN + 4]) != DHCP_MAGIC_COOKIE {
        return None;
    }
    let options = &bootp[BOOTP_FIXED_LEN + 4..];
    let msg_type = parse_option_53(options)?;

    Some(DhcpRequest {
        chaddr,
        xid,
        secs,
        broadcast,
        giaddr,
        msg_type,
    })
}

/// Walk the TLV options looking for option 53 (DHCP message type). Handles PAD
/// (0, no length) and stops at END (255). Returns `None` if absent/malformed.
fn parse_option_53(options: &[u8]) -> Option<u8> {
    let mut i = 0;
    while i < options.len() {
        let code = options[i];
        if code == OPT_END {
            break;
        }
        if code == OPT_PAD {
            i += 1;
            continue;
        }
        // code + len + value
        if i + 1 >= options.len() {
            return None;
        }
        let len = options[i + 1] as usize;
        let val_start = i + 2;
        let val_end = val_start.checked_add(len)?;
        if val_end > options.len() {
            return None;
        }
        if code == OPT_MESSAGE_TYPE {
            if len != 1 {
                return None;
            }
            return Some(options[val_start]);
        }
        i = val_end;
    }
    None
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

/// Build the full reply Ethernet frame for OFFER/ACK.
fn build_reply(
    req: &DhcpRequest,
    reply_type: u8,
    yiaddr: Ipv4Addr,
    subnet: &Subnet,
    dns_ip: Ipv4Addr,
) -> Vec<u8> {
    let bootp = build_bootp_reply(req, reply_type, yiaddr, subnet, dns_ip);

    // L2 destination: broadcast if the client asked for it (no IP yet), else
    // unicast to the client's MAC.
    let dst_mac = if req.broadcast {
        BROADCAST_MAC
    } else {
        req.chaddr
    };
    // L3 destination: same logic — a client without an IP cannot accept unicast
    // IP, so target the limited broadcast address.
    let dst_ip = if req.broadcast { BROADCAST_IP } else { yiaddr };
    let src_ip = subnet.gateway;

    let udp = build_udp(DHCP_SERVER_PORT, DHCP_CLIENT_PORT, &bootp);
    let ip = build_ipv4(src_ip, dst_ip, IP_PROTO_UDP, &udp);
    build_ethernet(GATEWAY_MAC, dst_mac, ETHERTYPE_IPV4, &ip)
}

/// Build the BOOTP reply (fixed header + magic cookie + DHCP options).
fn build_bootp_reply(
    req: &DhcpRequest,
    reply_type: u8,
    yiaddr: Ipv4Addr,
    subnet: &Subnet,
    dns_ip: Ipv4Addr,
) -> Vec<u8> {
    let mut b = vec![0u8; BOOTP_FIXED_LEN];
    b[0] = BOOTREPLY;
    b[1] = HTYPE_ETHERNET;
    b[2] = HLEN_ETHERNET;
    b[3] = 0; // hops
    b[4..8].copy_from_slice(&req.xid);
    b[8..10].copy_from_slice(&req.secs);
    // flags (10..12) echoed: preserve the broadcast bit the client set.
    let flags: u16 = if req.broadcast {
        BOOTP_FLAG_BROADCAST
    } else {
        0
    };
    b[10..12].copy_from_slice(&flags.to_be_bytes());
    // ciaddr (12..16) = 0. yiaddr (16..20) = the lease.
    b[16..20].copy_from_slice(&yiaddr.octets());
    // siaddr (20..24) = next server = our gateway.
    b[20..24].copy_from_slice(&subnet.gateway.octets());
    // giaddr (24..28) echoed.
    b[24..28].copy_from_slice(&req.giaddr);
    // chaddr (28..44): client MAC in the first 6 bytes.
    b[28..34].copy_from_slice(&req.chaddr);
    // sname (44..108) + file (108..236) stay zero.

    // Magic cookie + options.
    b.extend_from_slice(&DHCP_MAGIC_COOKIE.to_be_bytes());

    // 53: DHCP message type.
    push_opt(&mut b, OPT_MESSAGE_TYPE, &[reply_type]);
    // 54: server identifier = gateway.
    push_opt(&mut b, OPT_SERVER_ID, &subnet.gateway.octets());
    // 51: lease time.
    push_opt(&mut b, OPT_LEASE_TIME, &LEASE_SECS.to_be_bytes());
    // 1: subnet mask derived from the prefix.
    push_opt(
        &mut b,
        OPT_SUBNET_MASK,
        &prefix_to_mask(subnet.prefix).octets(),
    );
    // 3: router = gateway.
    push_opt(&mut b, OPT_ROUTER, &subnet.gateway.octets());
    // 6: DNS server.
    push_opt(&mut b, OPT_DNS, &dns_ip.octets());
    // 255: end.
    b.push(OPT_END);

    b
}

/// Append a TLV option (code, len, value).
fn push_opt(buf: &mut Vec<u8>, code: u8, value: &[u8]) {
    buf.push(code);
    buf.push(value.len() as u8);
    buf.extend_from_slice(value);
}

/// Build a UDP datagram (checksum left 0 — optional over IPv4, RFC 768).
fn build_udp(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let total = UDP_HDR_LEN + payload.len();
    let mut u = Vec::with_capacity(total);
    u.extend_from_slice(&src_port.to_be_bytes());
    u.extend_from_slice(&dst_port.to_be_bytes());
    u.extend_from_slice(&(total as u16).to_be_bytes());
    u.extend_from_slice(&[0, 0]); // checksum = 0 (disabled)
    u.extend_from_slice(payload);
    u
}

/// Build an IPv4 packet with a correct IHL, total length, and header checksum.
fn build_ipv4(src: Ipv4Addr, dst: Ipv4Addr, proto: u8, payload: &[u8]) -> Vec<u8> {
    let total_len = IPV4_MIN_HDR_LEN + payload.len();
    let mut h = Vec::with_capacity(total_len);
    h.push((4 << 4) | IPV4_MIN_IHL_WORDS); // version 4, IHL 5
    h.push(0); // DSCP/ECN
    h.extend_from_slice(&(total_len as u16).to_be_bytes());
    h.extend_from_slice(&[0, 0]); // identification
    h.extend_from_slice(&[0x40, 0x00]); // flags=DF, fragment offset 0
    h.push(64); // TTL
    h.push(proto);
    h.extend_from_slice(&[0, 0]); // checksum placeholder
    h.extend_from_slice(&src.octets());
    h.extend_from_slice(&dst.octets());

    let csum = ipv4_checksum(&h);
    h[10..12].copy_from_slice(&csum.to_be_bytes());

    h.extend_from_slice(payload);
    h
}

/// Build an Ethernet II frame.
fn build_ethernet(src: [u8; 6], dst: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut e = Vec::with_capacity(ETH_HDR_LEN + payload.len());
    e.extend_from_slice(&dst);
    e.extend_from_slice(&src);
    e.extend_from_slice(&ethertype.to_be_bytes());
    e.extend_from_slice(payload);
    e
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Standard one's-complement IPv4 header checksum (RFC 1071) over `header`,
/// whose checksum field must already be zeroed.
fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = header.chunks_exact(2);
    for c in &mut chunks {
        sum += u16::from_be_bytes([c[0], c[1]]) as u32;
    }
    if let [last] = chunks.remainder() {
        sum += (*last as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Convert a CIDR prefix length (0..=32) into a dotted netmask.
fn prefix_to_mask(prefix: u8) -> Ipv4Addr {
    let p = prefix.min(32);
    let bits: u32 = if p == 0 { 0 } else { u32::MAX << (32 - p) };
    Ipv4Addr::from(bits)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0xab, 0xcd, 0xef];
    const XID: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];

    fn test_subnet() -> Subnet {
        Subnet {
            base: Ipv4Addr::new(10, 69, 0, 0),
            prefix: 24,
            gateway: Ipv4Addr::new(10, 69, 0, 1),
        }
    }

    fn dns() -> Ipv4Addr {
        Ipv4Addr::new(10, 69, 0, 1)
    }

    fn leased_ip() -> Ipv4Addr {
        Ipv4Addr::new(10, 69, 0, 42)
    }

    /// Build a full Ethernet/IPv4/UDP/BOOTP DHCP request frame for `msg_type`,
    /// with the broadcast flag as given. Mirrors what `udhcpc`/`dhclient` emit.
    fn build_query(msg_type: u8, broadcast: bool) -> Vec<u8> {
        // BOOTP fixed header.
        let mut bootp = vec![0u8; BOOTP_FIXED_LEN];
        bootp[0] = BOOTREQUEST;
        bootp[1] = HTYPE_ETHERNET;
        bootp[2] = HLEN_ETHERNET;
        bootp[4..8].copy_from_slice(&XID);
        let flags: u16 = if broadcast { BOOTP_FLAG_BROADCAST } else { 0 };
        bootp[10..12].copy_from_slice(&flags.to_be_bytes());
        bootp[28..34].copy_from_slice(&CLIENT_MAC);
        // Magic cookie + options.
        bootp.extend_from_slice(&DHCP_MAGIC_COOKIE.to_be_bytes());
        push_opt(&mut bootp, OPT_MESSAGE_TYPE, &[msg_type]);
        // A PAD byte to exercise the parser's PAD handling, then END.
        bootp.push(OPT_PAD);
        bootp.push(OPT_END);

        let udp = build_udp(DHCP_CLIENT_PORT, DHCP_SERVER_PORT, &bootp);
        // Source 0.0.0.0 → 255.255.255.255 as a real bootstrapping client.
        let ip = build_ipv4(Ipv4Addr::UNSPECIFIED, BROADCAST_IP, IP_PROTO_UDP, &udp);
        build_ethernet(CLIENT_MAC, BROADCAST_MAC, ETHERTYPE_IPV4, &ip)
    }

    /// Decode the reply frame and return (message_type, yiaddr, server_id,
    /// router, dns, ipv4_checksum_ok), asserting structural validity en route.
    fn decode_reply(frame: &[u8]) -> (u8, Ipv4Addr, Ipv4Addr, Ipv4Addr, Ipv4Addr, bool) {
        // Ethernet.
        assert!(frame.len() > ETH_HDR_LEN);
        assert_eq!(be16(&frame[12..14]), ETHERTYPE_IPV4);
        // Source MAC must be our derived gateway MAC.
        assert_eq!(&frame[6..12], &GATEWAY_MAC);
        let l3 = &frame[ETH_HDR_LEN..];

        // IPv4.
        let ihl = (l3[0] & 0x0f) as usize * 4;
        assert_eq!(l3[0] >> 4, 4);
        assert_eq!(l3[9], IP_PROTO_UDP);
        let total_len = be16(&l3[2..4]) as usize;
        assert_eq!(total_len, l3.len());
        // Verify the header checksum: summing the header (incl. checksum) == 0.
        let checksum_ok = ipv4_checksum(&l3[..ihl]) == 0;
        let l4 = &l3[ihl..total_len];

        // UDP.
        assert_eq!(be16(&l4[0..2]), DHCP_SERVER_PORT);
        assert_eq!(be16(&l4[2..4]), DHCP_CLIENT_PORT);
        let udp_len = be16(&l4[4..6]) as usize;
        let bootp = &l4[UDP_HDR_LEN..udp_len];

        // BOOTP.
        assert_eq!(bootp[0], BOOTREPLY);
        assert_eq!(&bootp[4..8], &XID, "xid must be echoed");
        let mut yiaddr_b = [0u8; 4];
        yiaddr_b.copy_from_slice(&bootp[16..20]);
        let yiaddr = Ipv4Addr::from(yiaddr_b);
        assert_eq!(&bootp[28..34], &CLIENT_MAC, "chaddr echoed");
        assert_eq!(
            be32(&bootp[BOOTP_FIXED_LEN..BOOTP_FIXED_LEN + 4]),
            DHCP_MAGIC_COOKIE
        );

        // Options.
        let opts = &bootp[BOOTP_FIXED_LEN + 4..];
        let msg_type = find_opt(opts, OPT_MESSAGE_TYPE).expect("type")[0];
        let server_id =
            Ipv4Addr::from(<[u8; 4]>::try_from(find_opt(opts, OPT_SERVER_ID).unwrap()).unwrap());
        let router =
            Ipv4Addr::from(<[u8; 4]>::try_from(find_opt(opts, OPT_ROUTER).unwrap()).unwrap());
        let dns = Ipv4Addr::from(<[u8; 4]>::try_from(find_opt(opts, OPT_DNS).unwrap()).unwrap());
        // Lease time + subnet mask must be present and correct.
        let lease = find_opt(opts, OPT_LEASE_TIME).expect("lease");
        assert_eq!(be32(lease), LEASE_SECS);
        let mask = find_opt(opts, OPT_SUBNET_MASK).expect("mask");
        assert_eq!(
            Ipv4Addr::from(<[u8; 4]>::try_from(mask).unwrap()),
            Ipv4Addr::new(255, 255, 255, 0)
        );
        // Options must terminate with END.
        assert!(opts.contains(&OPT_END));

        (msg_type, yiaddr, server_id, router, dns, checksum_ok)
    }

    /// Find an option's value bytes by code (test-side TLV walker).
    fn find_opt(opts: &[u8], code: u8) -> Option<&[u8]> {
        let mut i = 0;
        while i < opts.len() {
            let c = opts[i];
            if c == OPT_END {
                break;
            }
            if c == OPT_PAD {
                i += 1;
                continue;
            }
            let len = opts[i + 1] as usize;
            let start = i + 2;
            if c == code {
                return Some(&opts[start..start + len]);
            }
            i = start + len;
        }
        None
    }

    fn seeded_store() -> LeaseStore {
        let mut s = LeaseStore::new();
        s.insert(CLIENT_MAC, leased_ip());
        s
    }

    #[test]
    fn discover_yields_offer() {
        let mut store = seeded_store();
        let frame = build_query(DHCP_DISCOVER, true);
        let reply = handle(&frame, &mut store, &test_subnet(), dns()).expect("offer");

        let (msg_type, yiaddr, server_id, router, dns_opt, csum_ok) = decode_reply(&reply);
        assert_eq!(msg_type, DHCP_OFFER, "option 53 == 2 (OFFER)");
        assert_eq!(yiaddr, leased_ip(), "yiaddr == pre-seeded lease");
        assert_eq!(server_id, test_subnet().gateway, "server-id == gateway");
        assert_eq!(router, test_subnet().gateway, "router == gateway");
        assert_eq!(dns_opt, dns(), "DNS == dns_ip");
        assert!(csum_ok, "IPv4 header checksum must verify");
    }

    #[test]
    fn request_yields_ack() {
        let mut store = seeded_store();
        let frame = build_query(DHCP_REQUEST, false);
        let reply = handle(&frame, &mut store, &test_subnet(), dns()).expect("ack");

        let (msg_type, yiaddr, server_id, _router, _dns, csum_ok) = decode_reply(&reply);
        assert_eq!(msg_type, DHCP_ACK, "option 53 == 5 (ACK)");
        assert_eq!(yiaddr, leased_ip());
        assert_eq!(server_id, test_subnet().gateway);
        assert!(csum_ok, "IPv4 header checksum must verify");
    }

    #[test]
    fn unicast_request_targets_client() {
        // With the broadcast flag clear, L2 dst must be the client MAC and L3
        // dst must be the leased IP (real dhclient renewing in-state).
        let mut store = seeded_store();
        let frame = build_query(DHCP_REQUEST, false);
        let reply = handle(&frame, &mut store, &test_subnet(), dns()).expect("ack");

        assert_eq!(&reply[0..6], &CLIENT_MAC, "L2 dst == client MAC (unicast)");
        let l3 = &reply[ETH_HDR_LEN..];
        assert_eq!(&l3[16..20], &leased_ip().octets(), "L3 dst == leased IP");
        assert_eq!(
            &l3[12..16],
            &test_subnet().gateway.octets(),
            "L3 src == gateway"
        );
    }

    #[test]
    fn broadcast_discover_targets_broadcast() {
        let mut store = seeded_store();
        let frame = build_query(DHCP_DISCOVER, true);
        let reply = handle(&frame, &mut store, &test_subnet(), dns()).expect("offer");

        assert_eq!(&reply[0..6], &BROADCAST_MAC, "L2 dst == ff:ff:ff:ff:ff:ff");
        let l3 = &reply[ETH_HDR_LEN..];
        assert_eq!(
            &l3[16..20],
            &BROADCAST_IP.octets(),
            "L3 dst == 255.255.255.255"
        );
    }

    #[test]
    fn non_dhcp_frame_returns_none() {
        let mut store = seeded_store();
        // Random bytes.
        let junk = [0xAB_u8; 60];
        assert!(handle(&junk, &mut store, &test_subnet(), dns()).is_none());

        // Too short to be an Ethernet header.
        let tiny = [0u8; 4];
        assert!(handle(&tiny, &mut store, &test_subnet(), dns()).is_none());

        // Valid DHCP DISCOVER but wrong ethertype (ARP) → None.
        let mut arp = build_query(DHCP_DISCOVER, true);
        arp[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
        assert!(handle(&arp, &mut store, &test_subnet(), dns()).is_none());
    }

    #[test]
    fn wrong_ports_return_none() {
        // A UDP/IPv4 frame that is not 68→67 (e.g. DNS 53) must be ignored.
        let mut store = seeded_store();
        let mut frame = build_query(DHCP_DISCOVER, true);
        // Flip the UDP dst port (offset: eth 14 + ip 20 + 2).
        let dport_off = ETH_HDR_LEN + IPV4_MIN_HDR_LEN + 2;
        frame[dport_off..dport_off + 2].copy_from_slice(&53u16.to_be_bytes());
        assert!(handle(&frame, &mut store, &test_subnet(), dns()).is_none());
    }

    #[test]
    fn unleased_mac_returns_none() {
        // Registry owns allocation: an unknown MAC gets no answer.
        let mut empty = LeaseStore::new();
        let frame = build_query(DHCP_DISCOVER, true);
        assert!(handle(&frame, &mut empty, &test_subnet(), dns()).is_none());
    }

    #[test]
    fn release_type_returns_none() {
        // Only DISCOVER/REQUEST are answered; other types (e.g. RELEASE=7).
        let mut store = seeded_store();
        let frame = build_query(7, false);
        assert!(handle(&frame, &mut store, &test_subnet(), dns()).is_none());
    }

    #[test]
    fn prefix_to_mask_cases() {
        assert_eq!(prefix_to_mask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(prefix_to_mask(16), Ipv4Addr::new(255, 255, 0, 0));
        assert_eq!(prefix_to_mask(8), Ipv4Addr::new(255, 0, 0, 0));
        assert_eq!(prefix_to_mask(0), Ipv4Addr::new(0, 0, 0, 0));
        assert_eq!(prefix_to_mask(32), Ipv4Addr::new(255, 255, 255, 255));
        assert_eq!(prefix_to_mask(30), Ipv4Addr::new(255, 255, 255, 252));
    }

    #[test]
    fn ipv4_checksum_known_vector() {
        // RFC-1071-style reference header (checksum field zeroed); the standard
        // worked example checksum is 0xb861.
        let hdr: [u8; 20] = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(ipv4_checksum(&hdr), 0xb861);
        // And a header carrying that checksum must sum to zero.
        let mut filled = hdr;
        filled[10..12].copy_from_slice(&0xb861u16.to_be_bytes());
        assert_eq!(ipv4_checksum(&filled), 0);
    }
}
