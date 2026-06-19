//! DHCP reply frame construction: BOOTP → UDP → IPv4 → Ethernet.

use crate::network::Subnet;
use std::net::Ipv4Addr;

use super::DhcpRequest;
use super::{
    BOOTP_FIXED_LEN, BOOTP_FLAG_BROADCAST, BOOTREPLY, BROADCAST_IP, BROADCAST_MAC,
    DHCP_CLIENT_PORT, DHCP_MAGIC_COOKIE, DHCP_SERVER_PORT, ETHERTYPE_IPV4, ETH_HDR_LEN,
    GATEWAY_MAC, HLEN_ETHERNET, HTYPE_ETHERNET, IPV4_MIN_HDR_LEN, IPV4_MIN_IHL_WORDS, IP_PROTO_UDP,
    LEASE_SECS, OPT_DNS, OPT_END, OPT_LEASE_TIME, OPT_MESSAGE_TYPE, OPT_ROUTER, OPT_SERVER_ID,
    OPT_SUBNET_MASK, UDP_HDR_LEN,
};

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

/// Build the full reply Ethernet frame for OFFER/ACK.
pub(super) fn build_reply(
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
pub(super) fn build_bootp_reply(
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

/// Standard one's-complement IPv4 header checksum (RFC 1071) over `header`,
/// whose checksum field must already be zeroed.
pub(super) fn ipv4_checksum(header: &[u8]) -> u16 {
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
pub(super) fn prefix_to_mask(prefix: u8) -> Ipv4Addr {
    let p = prefix.min(32);
    let bits: u32 = if p == 0 { 0 } else { u32::MAX << (32 - p) };
    Ipv4Addr::from(bits)
}
