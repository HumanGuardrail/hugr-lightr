// Tests for the dhcp module. Included as `#[cfg(test)] mod tests;` from mod.rs.
use super::*;
use super::build::{ipv4_checksum, prefix_to_mask};
use super::parse::{be16, be32};
use crate::network::Subnet;
use std::net::Ipv4Addr;

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
    push_opt_local(&mut bootp, OPT_MESSAGE_TYPE, &[msg_type]);
    // A PAD byte to exercise the parser's PAD handling, then END.
    bootp.push(OPT_PAD);
    bootp.push(OPT_END);

    let udp = build_udp_local(DHCP_CLIENT_PORT, DHCP_SERVER_PORT, &bootp);
    // Source 0.0.0.0 → 255.255.255.255 as a real bootstrapping client.
    let ip = build_ipv4_local(Ipv4Addr::UNSPECIFIED, BROADCAST_IP, IP_PROTO_UDP, &udp);
    build_ethernet_local(CLIENT_MAC, BROADCAST_MAC, ETHERTYPE_IPV4, &ip)
}

// Local frame-construction helpers (mirrors build module; needed to build client-side frames).
fn push_opt_local(buf: &mut Vec<u8>, code: u8, value: &[u8]) {
    buf.push(code);
    buf.push(value.len() as u8);
    buf.extend_from_slice(value);
}

fn build_udp_local(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let total = UDP_HDR_LEN + payload.len();
    let mut u = Vec::with_capacity(total);
    u.extend_from_slice(&src_port.to_be_bytes());
    u.extend_from_slice(&dst_port.to_be_bytes());
    u.extend_from_slice(&(total as u16).to_be_bytes());
    u.extend_from_slice(&[0, 0]);
    u.extend_from_slice(payload);
    u
}

fn build_ipv4_local(src: Ipv4Addr, dst: Ipv4Addr, proto: u8, payload: &[u8]) -> Vec<u8> {
    let total_len = IPV4_MIN_HDR_LEN + payload.len();
    let mut h = Vec::with_capacity(total_len);
    h.push((4 << 4) | IPV4_MIN_IHL_WORDS);
    h.push(0);
    h.extend_from_slice(&(total_len as u16).to_be_bytes());
    h.extend_from_slice(&[0, 0]);
    h.extend_from_slice(&[0x40, 0x00]);
    h.push(64);
    h.push(proto);
    h.extend_from_slice(&[0, 0]);
    h.extend_from_slice(&src.octets());
    h.extend_from_slice(&dst.octets());
    let csum = ipv4_checksum(&h);
    h[10..12].copy_from_slice(&csum.to_be_bytes());
    h.extend_from_slice(payload);
    h
}

fn build_ethernet_local(src: [u8; 6], dst: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut e = Vec::with_capacity(ETH_HDR_LEN + payload.len());
    e.extend_from_slice(&dst);
    e.extend_from_slice(&src);
    e.extend_from_slice(&ethertype.to_be_bytes());
    e.extend_from_slice(payload);
    e
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
