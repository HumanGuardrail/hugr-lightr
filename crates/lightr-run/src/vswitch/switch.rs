//! Pure L2 learning switch (ADR-0018, F-304 Phase-2).
//!
//! Parses raw Ethernet frames, learns `src MAC -> ingress port`, and decides
//! forwarding: known-unicast → that port; broadcast / multicast / unknown /
//! ARP → flood every other port. NO I/O — `forward` is a pure function over a
//! byte slice, so it is fully unit-testable with crafted frames (no VM).
//!
//! CONTRACT STUB (ADR-0018, WP-C2): signatures frozen; WP-C2 fills the bodies,
//! adds unit tests (crafted Ethernet + ARP frames), and REMOVES the `#![allow]`.

use std::collections::HashMap;
use std::net::Ipv4Addr;

/// A switch port == a member's socket index in the [`super::VSwitch`].
pub type PortId = usize;

/// MAC-learning table: source MAC → the port it was last seen on.
#[derive(Default)]
pub struct MacTable {
    map: HashMap<[u8; 6], PortId>,
}

impl MacTable {
    pub fn new() -> Self {
        Self::default()
    }
}

/// What the switch should do with one ingress frame.
#[derive(Debug, PartialEq, Eq)]
pub enum ForwardDecision {
    /// Deliver to exactly one known port.
    Unicast(PortId),
    /// Flood to all ports except the ingress (broadcast / multicast / unknown).
    Flood,
    /// Drop (malformed / too short to be a valid Ethernet frame).
    Drop,
}

/// Learn `frame`'s source on `from`, then decide how to forward it. Pure.
pub fn forward(frame: &[u8], from: PortId, table: &mut MacTable) -> ForwardDecision {
    // Minimum valid Ethernet frame: 6 (dst) + 6 (src) + 2 (ethertype) = 14 bytes.
    if frame.len() < 14 {
        return ForwardDecision::Drop;
    }

    let dst_mac: [u8; 6] = frame[0..6].try_into().unwrap();
    let src_mac: [u8; 6] = frame[6..12].try_into().unwrap();

    // Learn: record which port this source MAC arrived on (insert or update).
    table.map.insert(src_mac, from);

    // Decide forwarding based on destination MAC.
    // Broadcast: ff:ff:ff:ff:ff:ff
    let is_broadcast = dst_mac == [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    // Multicast: least-significant bit of the first octet is 1 (and not broadcast,
    // though broadcast also satisfies this — checking broadcast first is fine since
    // both produce Flood; we unify the check here for clarity).
    let is_multicast = (dst_mac[0] & 0x01) == 1;

    if is_broadcast || is_multicast {
        return ForwardDecision::Flood;
    }

    // Known unicast: dst MAC is in the table.
    if let Some(&port) = table.map.get(&dst_mac) {
        return ForwardDecision::Unicast(port);
    }

    // Unknown unicast: flood.
    ForwardDecision::Flood
}

/// If `frame` is an ARP request for `gateway_ip`, synthesize the ARP reply the
/// switch must answer with, sourced from `gateway_mac`. The embedded gateway is a
/// pure software endpoint (DHCP router + DNS server) with NO member port, so the
/// switch itself must answer ARP for it — otherwise a guest can never resolve the
/// nameserver's MAC and DNS-by-name (and any gateway-routed traffic) silently
/// fails. Returns `None` for any non-matching frame. Pure (no I/O).
pub fn arp_gateway_reply(
    frame: &[u8],
    gateway_ip: Ipv4Addr,
    gateway_mac: [u8; 6],
) -> Option<Vec<u8>> {
    // Ethernet header (14) + ARP IPv4-over-Ethernet packet (28) = 42 bytes.
    if frame.len() < 42 {
        return None;
    }
    // EtherType ARP (0x0806).
    if frame[12..14] != [0x08, 0x06] {
        return None;
    }
    // htype=Ethernet(1), ptype=IPv4(0x0800), hlen=6, plen=4, opcode=request(1).
    if frame[14..22] != [0x00, 0x01, 0x08, 0x00, 0x06, 0x04, 0x00, 0x01] {
        return None;
    }
    // Target protocol address (frame 38..42) must be the gateway IP.
    let tpa: [u8; 4] = frame[38..42].try_into().ok()?;
    if Ipv4Addr::from(tpa) != gateway_ip {
        return None;
    }
    // Requester == the ARP sender: HW at 22..28, proto IP at 28..32.
    let req_mac: [u8; 6] = frame[22..28].try_into().ok()?;
    let req_ip: [u8; 4] = frame[28..32].try_into().ok()?;
    let g_ip = gateway_ip.octets();

    let mut r = Vec::with_capacity(42);
    // Ethernet: dst = requester, src = gateway, EtherType ARP.
    r.extend_from_slice(&req_mac);
    r.extend_from_slice(&gateway_mac);
    r.extend_from_slice(&[0x08, 0x06]);
    // ARP reply: htype, ptype, hlen, plen, opcode=reply(2).
    r.extend_from_slice(&[0x00, 0x01, 0x08, 0x00, 0x06, 0x04, 0x00, 0x02]);
    r.extend_from_slice(&gateway_mac); // sender HW
    r.extend_from_slice(&g_ip); // sender proto
    r.extend_from_slice(&req_mac); // target HW
    r.extend_from_slice(&req_ip); // target proto
    Some(r)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 14-byte Ethernet frame with explicit dst + src MACs and a
    /// two-byte ethertype. `payload` allows callers to append further bytes (e.g.
    /// for ARP), but tests that only care about the L2 decision can pass `&[]`.
    fn make_frame(dst: [u8; 6], src: [u8; 6], ethertype: [u8; 2], payload: &[u8]) -> Vec<u8> {
        let mut f = Vec::with_capacity(14 + payload.len());
        f.extend_from_slice(&dst);
        f.extend_from_slice(&src);
        f.extend_from_slice(&ethertype);
        f.extend_from_slice(payload);
        f
    }

    const BCAST: [u8; 6] = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    const MAC_A: [u8; 6] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    const MAC_B: [u8; 6] = [0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
    // Multicast address: first octet LSB = 1.
    const MCAST: [u8; 6] = [0x01, 0x00, 0x5e, 0x00, 0x00, 0x01];
    const IPV4: [u8; 2] = [0x08, 0x00];
    const ARP: [u8; 2] = [0x08, 0x06];

    fn arp_request(target_ip: [u8; 4]) -> Vec<u8> {
        let mut arp = vec![0x00, 0x01, 0x08, 0x00, 0x06, 0x04, 0x00, 0x01];
        arp.extend_from_slice(&MAC_A); // sender HW
        arp.extend_from_slice(&[10, 69, 81, 2]); // sender proto
        arp.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // target HW (unknown)
        arp.extend_from_slice(&target_ip); // target proto
        make_frame(BCAST, MAC_A, ARP, &arp)
    }

    // ARP request for the gateway IP → well-formed reply from the gateway MAC;
    // a request for a non-gateway IP → None (falls through to L2 flood).
    #[test]
    fn arp_request_for_gateway_gets_reply() {
        let gw_ip = Ipv4Addr::new(10, 69, 81, 1);
        let gw_mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
        let reply = arp_gateway_reply(&arp_request([10, 69, 81, 1]), gw_ip, gw_mac)
            .expect("gateway ARP must be answered");
        assert_eq!(&reply[0..6], &MAC_A, "reply dst = requester");
        assert_eq!(&reply[6..12], &gw_mac, "reply src = gateway MAC");
        assert_eq!(&reply[12..14], &[0x08, 0x06], "ethertype ARP");
        assert_eq!(&reply[20..22], &[0x00, 0x02], "opcode = reply");
        assert_eq!(&reply[22..28], &gw_mac, "sender HW = gateway MAC");
        assert_eq!(
            &reply[28..32],
            &[10, 69, 81, 1],
            "sender proto = gateway IP"
        );
        assert_eq!(&reply[32..38], &MAC_A, "target HW = requester");
        // Non-gateway target is ignored.
        assert!(arp_gateway_reply(&arp_request([10, 69, 81, 9]), gw_ip, gw_mac).is_none());
    }

    // Test 1: broadcast destination → Flood.
    #[test]
    fn broadcast_dst_floods() {
        let mut table = MacTable::new();
        let frame = make_frame(BCAST, MAC_A, IPV4, &[]);
        assert_eq!(forward(&frame, 1, &mut table), ForwardDecision::Flood);
    }

    // Test 2: learn src on port 3, then a frame TO that MAC → Unicast(3).
    #[test]
    fn learn_then_unicast() {
        let mut table = MacTable::new();
        // First frame: src=MAC_A arriving on port 3 — teaches the table.
        let learn_frame = make_frame(BCAST, MAC_A, IPV4, &[]);
        forward(&learn_frame, 3, &mut table);

        // Second frame: dst=MAC_A should now resolve to port 3.
        let lookup_frame = make_frame(MAC_A, MAC_B, IPV4, &[]);
        assert_eq!(
            forward(&lookup_frame, 1, &mut table),
            ForwardDecision::Unicast(3)
        );
    }

    // Test 3: unknown unicast destination → Flood.
    #[test]
    fn unknown_unicast_floods() {
        let mut table = MacTable::new();
        let frame = make_frame(MAC_B, MAC_A, IPV4, &[]);
        // MAC_B has never been seen as a source, so it is unknown.
        assert_eq!(forward(&frame, 2, &mut table), ForwardDecision::Flood);
    }

    // Test 4: multicast destination → Flood.
    #[test]
    fn multicast_dst_floods() {
        let mut table = MacTable::new();
        let frame = make_frame(MCAST, MAC_A, IPV4, &[]);
        assert_eq!(forward(&frame, 0, &mut table), ForwardDecision::Flood);
    }

    // Test 5: frame shorter than 14 bytes → Drop.
    #[test]
    fn short_frame_drops() {
        let mut table = MacTable::new();
        let frame = vec![0u8; 13];
        assert_eq!(forward(&frame, 0, &mut table), ForwardDecision::Drop);
    }

    // Test 5b: empty frame → Drop.
    #[test]
    fn empty_frame_drops() {
        let mut table = MacTable::new();
        assert_eq!(forward(&[], 0, &mut table), ForwardDecision::Drop);
    }

    // Test 6: ARP request (broadcast dst, ethertype 0x0806) → Flood.
    #[test]
    fn arp_request_floods() {
        let mut table = MacTable::new();
        // ARP requests are always broadcast at L2.
        // Minimal ARP payload (28 bytes for IPv4 ARP) appended for realism.
        let arp_payload = [
            0x00, 0x01, // hardware type: Ethernet
            0x08, 0x00, // protocol type: IPv4
            0x06, // hardware address length
            0x04, // protocol address length
            0x00, 0x01, // operation: request
            // sender MAC + IP, target MAC + IP (zeros for brevity)
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0xc0, 0xa8, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0xc0, 0xa8, 0x01, 0x02,
        ];
        let frame = make_frame(BCAST, MAC_A, ARP, &arp_payload);
        assert_eq!(forward(&frame, 4, &mut table), ForwardDecision::Flood);
    }
}
