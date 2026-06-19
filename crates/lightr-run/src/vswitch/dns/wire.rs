//! DNS wire-frame builders — extracted from `mod.rs` (pure move, no logic change).
//!
//! Public to the parent module only: `build_response` and `build_nodata` are
//! called by `handle`; `encapsulate` and `ipv4_checksum` are wire-internal.

use std::net::Ipv4Addr;

use super::{
    ParsedQuery, ETH_HDR_LEN, ETH_TYPE_IPV4, IP_PROTO_UDP, IP_VERSION_4, DNS_PORT, ANSWER_TTL,
    QTYPE_A, QCLASS_IN,
};

/// Build the full reply frame (Ethernet/IPv4/UDP/DNS) answering `q` with `ip`.
pub(super) fn build_response(q: &ParsedQuery, ip: Ipv4Addr) -> Vec<u8> {
    // ── DNS payload ──────────────────────────────────────────────────────────
    let mut dns = Vec::with_capacity(super::DNS_HDR_LEN + q.question.len() + 16);
    dns.extend_from_slice(&q.id.to_be_bytes());
    // Flags: QR=1, Opcode=0, AA=0, TC=0, RD=0, RA=1, RCODE=0.
    //   0x8000 (QR) | 0x0080 (RA) = 0x8080.
    dns.extend_from_slice(&0x8080u16.to_be_bytes());
    dns.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    dns.extend_from_slice(&1u16.to_be_bytes()); // ANCOUNT
    dns.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    dns.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
                                                // Echo the question verbatim.
    dns.extend_from_slice(&q.question);
    // Answer: NAME = compression pointer to the question's QNAME at offset 12.
    dns.extend_from_slice(&0xC00Cu16.to_be_bytes());
    dns.extend_from_slice(&QTYPE_A.to_be_bytes());
    dns.extend_from_slice(&QCLASS_IN.to_be_bytes());
    dns.extend_from_slice(&ANSWER_TTL.to_be_bytes());
    dns.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
    dns.extend_from_slice(&ip.octets()); // RDATA

    // The reply's IP src is the query's IP dst (the gateway/DNS server) and
    // vice-versa; same swap for UDP ports and Ethernet MACs.
    encapsulate(
        &dns, q.dst_mac,  // reply src MAC = query dst MAC
        q.src_mac,  // reply dst MAC = query src MAC
        q.dst_ip,   // reply src IP  = query dst IP (the gateway)
        q.src_ip,   // reply dst IP  = query src IP (the guest)
        q.src_port, // reply dst port = query src port
    )
}

/// Build a NOERROR / empty-answer (NODATA) reply: the queried name EXISTS but
/// has no record of the requested type. Header QR=1, RA=1, RCODE=0, ANCOUNT=0;
/// the question is echoed (some resolvers require it). This is the correct
/// answer for an AAAA query on an IPv4-only mesh name — see [`super::handle`].
pub(super) fn build_nodata(q: &ParsedQuery) -> Vec<u8> {
    let mut dns = Vec::with_capacity(super::DNS_HDR_LEN + q.question.len());
    dns.extend_from_slice(&q.id.to_be_bytes());
    // QR=1, RA=1, RCODE=0 (NOERROR) → 0x8080.
    dns.extend_from_slice(&0x8080u16.to_be_bytes());
    dns.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    dns.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT = 0  (NODATA)
    dns.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    dns.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    dns.extend_from_slice(&q.question); // echo the question

    encapsulate(&dns, q.dst_mac, q.src_mac, q.dst_ip, q.src_ip, q.src_port)
}

/// Wrap a DNS payload in UDP(53→dst_port)/IPv4/Ethernet with a valid IPv4
/// header checksum. UDP checksum is left 0 (legal for IPv4/UDP).
pub(super) fn encapsulate(
    dns: &[u8],
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    dst_port: u16,
) -> Vec<u8> {
    let udp_len = 8 + dns.len();
    let ip_total = 20 + udp_len;

    let mut out = Vec::with_capacity(ETH_HDR_LEN + ip_total);

    // ── Ethernet ───────────────────────────────────────────────────────────
    out.extend_from_slice(&dst_mac);
    out.extend_from_slice(&src_mac);
    out.extend_from_slice(&ETH_TYPE_IPV4.to_be_bytes());

    // ── IPv4 (20-byte header, no options) ────────────────────────────────────
    let ip_start = out.len();
    out.push((IP_VERSION_4 << 4) | 5); // version 4, IHL 5
    out.push(0); // DSCP/ECN
    out.extend_from_slice(&(ip_total as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // identification
    out.extend_from_slice(&0x4000u16.to_be_bytes()); // flags: DF set, offset 0
    out.push(64); // TTL
    out.push(IP_PROTO_UDP);
    out.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out.extend_from_slice(&src_ip.octets());
    out.extend_from_slice(&dst_ip.octets());
    // Compute + patch the header checksum.
    let csum = ipv4_checksum(&out[ip_start..ip_start + 20]);
    out[ip_start + 10..ip_start + 12].copy_from_slice(&csum.to_be_bytes());

    // ── UDP ──────────────────────────────────────────────────────────────────
    out.extend_from_slice(&DNS_PORT.to_be_bytes()); // src port = 53
    out.extend_from_slice(&dst_port.to_be_bytes());
    out.extend_from_slice(&(udp_len as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // checksum 0 (optional for IPv4)

    // ── DNS payload ──────────────────────────────────────────────────────────
    out.extend_from_slice(dns);

    out
}

/// One's-complement sum over a 20-byte IPv4 header (checksum field assumed 0).
fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < header.len() {
        sum += u16::from_be_bytes([header[i], header[i + 1]]) as u32;
        i += 2;
    }
    if i < header.len() {
        // Odd trailing byte (won't happen for a 20-byte header, but be safe).
        sum += (header[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
