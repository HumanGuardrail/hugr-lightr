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
