//! Minimal embedded DNS responder (ADR-0018, F-304 Phase-2).
//!
//! Answers A queries for container / service names (and aliases) from the
//! network's name table; forwards everything else to the host upstream resolver
//! when one is provided. This is what makes `curl http://web` resolve. Frame-in
//! / frame-out, pure parse/build — unit-testable with captured queries (no VM).
//!
//! CONTRACT STUB (ADR-0018, WP-C4): signatures frozen; WP-C4 fills the bodies,
//! adds unit tests, and REMOVES the `#![allow]`.
//!
//! ## Not-found policy
//!
//! When a queried name is *not* in `names`:
//! * `upstream = Some(ip)` → relay the query verbatim to `ip:53` over a
//!   fresh UDP socket with a bounded (≤2 s) timeout and splice the upstream
//!   answer back as a frame. This is the single point of real I/O in the
//!   module; everything else is pure parse/build.
//! * `upstream = None` → return `None`. We deliberately do **not** synthesize
//!   NXDOMAIN: with no resolver configured the switch should stay transparent
//!   and let the frame flood/forward as ordinary traffic rather than poison a
//!   name that some *other* (e.g. external) resolver could answer. Returning a
//!   forged NXDOMAIN would also negatively cache the name in the guest's
//!   resolver, which is worse than a silent miss for a best-effort embedded DNS.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

/// name (and each alias) → IP, built from the network registry's members.
pub type NameTable = HashMap<String, Ipv4Addr>;

// ── Wire-format constants ───────────────────────────────────────────────────

const ETH_HDR_LEN: usize = 14;
const ETH_TYPE_IPV4: u16 = 0x0800;

const IP_PROTO_UDP: u8 = 17;
const IP_VERSION_4: u8 = 4;

const DNS_PORT: u16 = 53;
const DNS_HDR_LEN: usize = 12;

const QTYPE_A: u16 = 1;
const QCLASS_IN: u16 = 1;

/// TTL handed out for answers we synthesize from the name table (seconds).
const ANSWER_TTL: u32 = 60;

/// Bound on the upstream relay round-trip.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);

/// Handle one inbound DNS query frame (full Ethernet/IP/UDP/DNS). Answers A
/// records found in `names`; otherwise forwards to `upstream` if `Some`.
/// Returns the reply FRAME, or `None` if not a query we handle.
pub fn handle(frame: &[u8], names: &NameTable, upstream: Option<Ipv4Addr>) -> Option<Vec<u8>> {
    // ── Peel Ethernet → IPv4 → UDP → DNS, validating as we go. ──────────────
    let parsed = parse_query(frame)?;

    // We only synthesize answers for A / IN questions. Anything else we either
    // relay upstream (so e.g. AAAA still works through the host resolver) or,
    // with no upstream, leave alone.
    let answerable = parsed.qtype == QTYPE_A && parsed.qclass == QCLASS_IN;

    if answerable {
        let key = normalize_name(&parsed.qname);
        if let Some(&ip) = names.get(&key) {
            return Some(build_response(&parsed, ip));
        }
    }

    // Not in the table (or not an A/IN question): forward upstream if we can.
    match upstream {
        Some(server) => relay_upstream(frame, &parsed, server),
        None => None,
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// Everything we need from the inbound query to (a) look the name up and (b)
/// build a correctly-addressed reply frame.
struct ParsedQuery {
    // L2
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    // L3
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    // L4
    src_port: u16,
    // DNS
    id: u16,
    qname: String,
    qtype: u16,
    qclass: u16,
    /// The raw question section (QNAME+QTYPE+QCLASS) so the reply can echo it
    /// byte-for-byte and the answer's compression pointer (0xC00C) stays valid.
    question: Vec<u8>,
}

fn read_u16(buf: &[u8], at: usize) -> Option<u16> {
    buf.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn parse_query(frame: &[u8]) -> Option<ParsedQuery> {
    // ── Ethernet ───────────────────────────────────────────────────────────
    if frame.len() < ETH_HDR_LEN {
        return None;
    }
    let mut dst_mac = [0u8; 6];
    let mut src_mac = [0u8; 6];
    dst_mac.copy_from_slice(&frame[0..6]);
    src_mac.copy_from_slice(&frame[6..12]);
    if read_u16(frame, 12)? != ETH_TYPE_IPV4 {
        return None;
    }

    // ── IPv4 ─────────────────────────────────────────────────────────────────
    let ip = &frame[ETH_HDR_LEN..];
    let vihl = *ip.first()?;
    if (vihl >> 4) != IP_VERSION_4 {
        return None;
    }
    let ihl = ((vihl & 0x0f) as usize) * 4;
    if ihl < 20 || ip.len() < ihl {
        return None;
    }
    if ip[9] != IP_PROTO_UDP {
        return None;
    }
    // total length bounds the L3 payload (guards against trailing padding).
    let ip_total = read_u16(ip, 2)? as usize;
    if ip_total < ihl || ip_total > ip.len() {
        return None;
    }
    let mut src_ip = [0u8; 4];
    let mut dst_ip = [0u8; 4];
    src_ip.copy_from_slice(&ip[12..16]);
    dst_ip.copy_from_slice(&ip[16..20]);

    // ── UDP ──────────────────────────────────────────────────────────────────
    let udp = &ip[ihl..ip_total];
    if udp.len() < 8 {
        return None;
    }
    let src_port = read_u16(udp, 0)?;
    let dst_port = read_u16(udp, 2)?;
    if dst_port != DNS_PORT {
        return None;
    }
    let udp_len = read_u16(udp, 4)? as usize;
    if udp_len < 8 || udp_len > udp.len() {
        return None;
    }

    // ── DNS ──────────────────────────────────────────────────────────────────
    let dns = &udp[8..udp_len];
    if dns.len() < DNS_HDR_LEN {
        return None;
    }
    let id = read_u16(dns, 0)?;
    let flags = read_u16(dns, 2)?;
    // Must be a query (QR=0) and an opcode we understand (standard QUERY=0).
    if flags & 0x8000 != 0 {
        return None;
    }
    let opcode = (flags >> 11) & 0x0f;
    if opcode != 0 {
        return None;
    }
    let qdcount = read_u16(dns, 4)?;
    if qdcount != 1 {
        return None;
    }

    // Question: QNAME labels, then QTYPE, QCLASS.
    let q_start = DNS_HDR_LEN;
    let (qname, after_name) = parse_qname(dns, q_start)?;
    let qtype = read_u16(dns, after_name)?;
    let qclass = read_u16(dns, after_name + 2)?;
    let q_end = after_name + 4;
    let question = dns.get(q_start..q_end)?.to_vec();

    Some(ParsedQuery {
        src_mac,
        dst_mac,
        src_ip: Ipv4Addr::from(src_ip),
        dst_ip: Ipv4Addr::from(dst_ip),
        src_port,
        id,
        qname,
        qtype,
        qclass,
        question,
    })
}

/// Parse an uncompressed QNAME starting at `start`, returning the dotted name
/// (without the trailing root dot) and the offset just past the terminating
/// zero length octet. Rejects compression pointers in the *question* (a query
/// QNAME is never compressed) and malformed/oversized labels.
fn parse_qname(dns: &[u8], start: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut pos = start;
    loop {
        let len = *dns.get(pos)? as usize;
        if len == 0 {
            return Some((name, pos + 1));
        }
        // Top two bits set ⇒ compression pointer; not valid in a question.
        if len & 0xc0 != 0 {
            return None;
        }
        pos += 1;
        let label = dns.get(pos..pos + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        // Labels are octets; DNS names are case-insensitive ASCII. Push raw
        // bytes as chars (lossless for the ASCII the resolver will send).
        for &b in label {
            name.push(b as char);
        }
        pos += len;
        // RFC 1035: names ≤ 255 octets. Guard the accumulator.
        if name.len() > 255 {
            return None;
        }
    }
}

/// Lowercase + strip a single trailing dot for table lookup. (parse_qname
/// already drops the root dot, but be defensive.)
fn normalize_name(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

// ── Response building ────────────────────────────────────────────────────────

/// Build the full reply frame (Ethernet/IPv4/UDP/DNS) answering `q` with `ip`.
fn build_response(q: &ParsedQuery, ip: Ipv4Addr) -> Vec<u8> {
    // ── DNS payload ──────────────────────────────────────────────────────────
    let mut dns = Vec::with_capacity(DNS_HDR_LEN + q.question.len() + 16);
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

/// Wrap a DNS payload in UDP(53→dst_port)/IPv4/Ethernet with a valid IPv4
/// header checksum. UDP checksum is left 0 (legal for IPv4/UDP).
fn encapsulate(
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

// ── Upstream relay (the one allowed I/O path) ────────────────────────────────

/// Relay the original DNS payload to `server:53`, wait (bounded) for the reply,
/// and re-encapsulate it as a frame back to the querying guest. Any failure
/// (bind/connect/timeout/short reply) collapses to `None` so the switch simply
/// drops the query rather than forging an answer.
fn relay_upstream(frame: &[u8], q: &ParsedQuery, server: Ipv4Addr) -> Option<Vec<u8>> {
    let dns_payload = extract_dns_payload(frame)?;

    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    sock.set_read_timeout(Some(UPSTREAM_TIMEOUT)).ok()?;
    sock.set_write_timeout(Some(UPSTREAM_TIMEOUT)).ok()?;
    let dest = SocketAddr::from((server, DNS_PORT));
    sock.send_to(dns_payload, dest).ok()?;

    let mut buf = [0u8; 1500];
    let (n, _from) = sock.recv_from(&mut buf).ok()?;
    if n < DNS_HDR_LEN {
        return None;
    }
    // Sanity: the answer must carry our transaction id.
    if read_u16(&buf, 0)? != q.id {
        return None;
    }

    Some(encapsulate(
        &buf[..n],
        q.dst_mac,
        q.src_mac,
        q.dst_ip,
        q.src_ip,
        q.src_port,
    ))
}

/// Re-walk the headers to slice out just the DNS payload (UDP body) for relay.
fn extract_dns_payload(frame: &[u8]) -> Option<&[u8]> {
    let ip = frame.get(ETH_HDR_LEN..)?;
    let ihl = ((*ip.first()? & 0x0f) as usize) * 4;
    let ip_total = read_u16(ip, 2)? as usize;
    if ip_total > ip.len() || ihl < 20 || ip_total < ihl + 8 {
        return None;
    }
    let udp = &ip[ihl..ip_total];
    let udp_len = read_u16(udp, 4)? as usize;
    if udp_len < 8 || udp_len > udp.len() {
        return None;
    }
    Some(&udp[8..udp_len])
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    const GUEST_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    const GW_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0xfe];
    const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 5);
    const GW_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
    const CLIENT_PORT: u16 = 0xC001;
    const QUERY_ID: u16 = 0x1234;

    /// Build an A-query frame for `name` (Ethernet/IPv4/UDP/DNS), guest→gateway.
    fn build_query_frame(name: &str) -> Vec<u8> {
        // DNS payload.
        let mut dns = Vec::new();
        dns.extend_from_slice(&QUERY_ID.to_be_bytes());
        dns.extend_from_slice(&0x0100u16.to_be_bytes()); // RD=1, QR=0, opcode 0
        dns.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        dns.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
        dns.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        dns.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
        for label in name.split('.') {
            dns.push(label.len() as u8);
            dns.extend_from_slice(label.as_bytes());
        }
        dns.push(0); // root
        dns.extend_from_slice(&QTYPE_A.to_be_bytes());
        dns.extend_from_slice(&QCLASS_IN.to_be_bytes());

        encapsulate_for_test(&dns, GUEST_MAC, GW_MAC, GUEST_IP, GW_IP, CLIENT_PORT)
    }

    /// Like `encapsulate` but src_port = CLIENT_PORT and dst_port = 53 (a query
    /// direction), so we exercise the real parser against a realistic frame.
    fn encapsulate_for_test(
        dns: &[u8],
        src_mac: [u8; 6],
        dst_mac: [u8; 6],
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        src_port: u16,
    ) -> Vec<u8> {
        let udp_len = 8 + dns.len();
        let ip_total = 20 + udp_len;
        let mut out = Vec::new();
        out.extend_from_slice(&dst_mac);
        out.extend_from_slice(&src_mac);
        out.extend_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        let ip_start = out.len();
        out.push((IP_VERSION_4 << 4) | 5);
        out.push(0);
        out.extend_from_slice(&(ip_total as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0x4000u16.to_be_bytes());
        out.push(64);
        out.push(IP_PROTO_UDP);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&src_ip.octets());
        out.extend_from_slice(&dst_ip.octets());
        let csum = ipv4_checksum(&out[ip_start..ip_start + 20]);
        out[ip_start + 10..ip_start + 12].copy_from_slice(&csum.to_be_bytes());
        out.extend_from_slice(&src_port.to_be_bytes());
        out.extend_from_slice(&DNS_PORT.to_be_bytes());
        out.extend_from_slice(&(udp_len as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(dns);
        out
    }

    fn table_with(name: &str, ip: Ipv4Addr) -> NameTable {
        let mut t = NameTable::new();
        t.insert(name.to_string(), ip);
        t
    }

    #[test]
    fn dns_answers_known_name_with_a_record() {
        let target = Ipv4Addr::new(10, 0, 0, 42);
        let names = table_with("web", target);
        let query = build_query_frame("web");

        let reply = handle(&query, &names, None).expect("known name must be answered");

        // ── Ethernet: MACs swapped relative to the query. ────────────────────
        assert_eq!(&reply[0..6], &GUEST_MAC, "reply dst MAC = query src MAC");
        assert_eq!(&reply[6..12], &GW_MAC, "reply src MAC = query dst MAC");
        assert_eq!(read_u16(&reply, 12).unwrap(), ETH_TYPE_IPV4);

        // ── IPv4: addresses swapped, version 4, proto UDP, checksum valid. ────
        let ip = &reply[ETH_HDR_LEN..];
        assert_eq!(ip[0] >> 4, 4);
        let ihl = ((ip[0] & 0x0f) as usize) * 4;
        assert_eq!(ihl, 20);
        assert_eq!(ip[9], IP_PROTO_UDP);
        assert_eq!(&ip[12..16], &GW_IP.octets(), "reply src IP = gateway");
        assert_eq!(&ip[16..20], &GUEST_IP.octets(), "reply dst IP = guest");
        // The on-wire checksum is correct iff the one's-complement sum over the
        // whole header (checksum field included) is 0xFFFF.
        let mut sum: u32 = 0;
        let mut i = 0;
        while i < ihl {
            sum += u16::from_be_bytes([ip[i], ip[i + 1]]) as u32;
            i += 2;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        assert_eq!(sum as u16, 0xffff, "IPv4 header checksum must verify");

        // ── UDP: src 53, dst = the client's ephemeral port. ──────────────────
        let udp = &ip[ihl..];
        assert_eq!(read_u16(udp, 0).unwrap(), DNS_PORT, "reply src port = 53");
        assert_eq!(read_u16(udp, 2).unwrap(), CLIENT_PORT, "reply dst = client");

        // ── DNS: same id, QR + RA set, one question + one answer. ────────────
        let dns = &udp[8..];
        assert_eq!(read_u16(dns, 0).unwrap(), QUERY_ID, "transaction id echoed");
        let flags = read_u16(dns, 2).unwrap();
        assert_eq!(flags & 0x8000, 0x8000, "QR must be set (response)");
        assert_eq!(flags & 0x0080, 0x0080, "RA must be set");
        assert_eq!(read_u16(dns, 4).unwrap(), 1, "QDCOUNT");
        assert_eq!(read_u16(dns, 6).unwrap(), 1, "ANCOUNT");

        // Answer starts right after the echoed question. Question for "web" is
        // 1+3 (label) +1 (root) +2 (QTYPE) +2 (QCLASS) = 9 bytes after header.
        let ans = &dns[DNS_HDR_LEN + 9..];
        assert_eq!(
            read_u16(ans, 0).unwrap(),
            0xC00C,
            "name compression pointer"
        );
        assert_eq!(read_u16(ans, 2).unwrap(), QTYPE_A, "TYPE A");
        assert_eq!(read_u16(ans, 4).unwrap(), QCLASS_IN, "CLASS IN");
        assert_eq!(
            u32::from_be_bytes([ans[6], ans[7], ans[8], ans[9]]),
            ANSWER_TTL,
            "TTL"
        );
        assert_eq!(read_u16(ans, 10).unwrap(), 4, "RDLENGTH = 4");
        assert_eq!(&ans[12..16], &target.octets(), "RDATA = the looked-up IP");
    }

    #[test]
    fn dns_lookup_is_case_insensitive_and_dot_tolerant() {
        // Table holds the canonical lowercase key; query arrives upper-cased.
        let target = Ipv4Addr::new(10, 0, 0, 7);
        let names = table_with("api", target);
        let query = build_query_frame("API");

        let reply = handle(&query, &names, None).expect("case-insensitive match");
        // Pull RDATA out of the answer (question "API" → 1+3+1+2+2 = 9 bytes).
        let ip = &reply[ETH_HDR_LEN..];
        let ihl = ((ip[0] & 0x0f) as usize) * 4;
        let dns = &ip[ihl + 8..];
        let ans = &dns[DNS_HDR_LEN + 9..];
        assert_eq!(&ans[12..16], &target.octets());

        // And normalize_name handles a trailing dot directly.
        assert_eq!(normalize_name("Web.Local."), "web.local");
    }

    #[test]
    fn dns_unknown_name_without_upstream_is_none() {
        let names = table_with("web", Ipv4Addr::new(10, 0, 0, 42));
        let query = build_query_frame("nope");
        // Policy: no upstream ⇒ stay transparent, return None (documented).
        assert_eq!(handle(&query, &names, None), None);
    }

    #[test]
    fn non_dns_frame_is_none() {
        let names = table_with("web", Ipv4Addr::new(10, 0, 0, 42));

        // (a) Too short to be Ethernet.
        assert_eq!(handle(&[0u8; 8], &names, None), None);

        // (b) Wrong ethertype (ARP 0x0806) over a 60-byte frame.
        let mut arp = vec![0u8; 60];
        arp[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
        assert_eq!(handle(&arp, &names, None), None);

        // (c) Valid IPv4/UDP but to port 80, not 53 → not ours.
        let mut dns = Vec::new();
        dns.extend_from_slice(&QUERY_ID.to_be_bytes());
        dns.extend_from_slice(&0x0100u16.to_be_bytes());
        dns.extend_from_slice(&1u16.to_be_bytes());
        dns.extend_from_slice(&[0u8; 6]); // an/ns/ar counts
        dns.extend_from_slice(b"\x03web\x00");
        dns.extend_from_slice(&QTYPE_A.to_be_bytes());
        dns.extend_from_slice(&QCLASS_IN.to_be_bytes());
        let mut frame = encapsulate_for_test(&dns, GUEST_MAC, GW_MAC, GUEST_IP, GW_IP, CLIENT_PORT);
        // Patch UDP dst port (offset: 14 eth + 20 ip + 2) from 53 → 80.
        let dport = ETH_HDR_LEN + 20 + 2;
        frame[dport..dport + 2].copy_from_slice(&80u16.to_be_bytes());
        assert_eq!(handle(&frame, &names, None), None);
    }

    #[test]
    fn dns_non_a_query_without_upstream_is_none() {
        // An AAAA (type 28) query for a known name is not synthesizable here;
        // with no upstream it must be None rather than a forged A answer.
        let names = table_with("web", Ipv4Addr::new(10, 0, 0, 42));
        let mut dns = Vec::new();
        dns.extend_from_slice(&QUERY_ID.to_be_bytes());
        dns.extend_from_slice(&0x0100u16.to_be_bytes());
        dns.extend_from_slice(&1u16.to_be_bytes());
        dns.extend_from_slice(&[0u8; 6]);
        dns.extend_from_slice(b"\x03web\x00");
        dns.extend_from_slice(&28u16.to_be_bytes()); // QTYPE AAAA
        dns.extend_from_slice(&QCLASS_IN.to_be_bytes());
        let frame = encapsulate_for_test(&dns, GUEST_MAC, GW_MAC, GUEST_IP, GW_IP, CLIENT_PORT);
        assert_eq!(handle(&frame, &names, None), None);
    }
}
