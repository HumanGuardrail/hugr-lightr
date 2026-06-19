use super::wire::ipv4_checksum;
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

/// Build an AAAA (type 28) query frame for `name`, guest→gateway.
fn build_aaaa_query_frame(name: &str) -> Vec<u8> {
    let mut dns = Vec::new();
    dns.extend_from_slice(&QUERY_ID.to_be_bytes());
    dns.extend_from_slice(&0x0100u16.to_be_bytes());
    dns.extend_from_slice(&1u16.to_be_bytes());
    dns.extend_from_slice(&[0u8; 6]);
    for label in name.split('.') {
        dns.push(label.len() as u8);
        dns.extend_from_slice(label.as_bytes());
    }
    dns.push(0);
    dns.extend_from_slice(&QTYPE_AAAA.to_be_bytes());
    dns.extend_from_slice(&QCLASS_IN.to_be_bytes());
    encapsulate_for_test(&dns, GUEST_MAC, GW_MAC, GUEST_IP, GW_IP, CLIENT_PORT)
}

#[test]
fn dns_aaaa_for_owned_name_is_nodata_not_nxdomain() {
    // CRITICAL musl-compat contract (WP-C10 E2E finding): a guest's
    // getaddrinfo() fires A + AAAA in parallel; if the AAAA for an owned,
    // IPv4-only mesh name comes back NXDOMAIN (or unanswered → upstream
    // NXDOMAIN), musl discards the valid A answer and the lookup fails with
    // "bad address". So an AAAA for a KNOWN name must be NOERROR/NODATA:
    // QR=1, RCODE=0, ANCOUNT=0, the question echoed.
    let names = table_with("web", Ipv4Addr::new(10, 0, 0, 42));
    let frame = build_aaaa_query_frame("web");

    // Even WITH an upstream set, an owned name must be answered locally as
    // NODATA — never relayed (the upstream would say NXDOMAIN).
    let reply = handle(&frame, &names, Some(Ipv4Addr::new(8, 8, 8, 8)))
        .expect("owned AAAA must get a local NODATA answer, not None/relay");

    let ip = &reply[ETH_HDR_LEN..];
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    let dns = &ip[ihl + 8..];
    assert_eq!(read_u16(dns, 0).unwrap(), QUERY_ID, "transaction id echoed");
    let flags = read_u16(dns, 2).unwrap();
    assert_eq!(flags & 0x8000, 0x8000, "QR set (response)");
    assert_eq!(flags & 0x000f, 0, "RCODE = 0 (NOERROR), NOT NXDOMAIN(3)");
    assert_eq!(read_u16(dns, 4).unwrap(), 1, "QDCOUNT = 1");
    assert_eq!(read_u16(dns, 6).unwrap(), 0, "ANCOUNT = 0 (NODATA)");
    // Question echoed: "web" → 1+3+1+2+2 = 9 bytes; nothing after it.
    assert_eq!(dns.len(), DNS_HDR_LEN + 9, "question echoed, no answer RRs");
}

#[test]
fn dns_aaaa_for_unowned_name_without_upstream_is_none() {
    // For a name we do NOT own, with no upstream, stay transparent (None) —
    // we must not forge a NODATA for a name that isn't ours.
    let names = table_with("web", Ipv4Addr::new(10, 0, 0, 42));
    let frame = build_aaaa_query_frame("nope");
    assert_eq!(handle(&frame, &names, None), None);
}
