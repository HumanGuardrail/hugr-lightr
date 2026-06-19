//! Pure L2 learning switch (ADR-0018, F-304 Phase-2).
//!
//! Parses raw Ethernet frames, learns `src MAC -> ingress port`, and decides
//! forwarding: known-unicast → that port; broadcast / multicast / unknown /
//! ARP → flood every other port. NO I/O — `forward` is a pure function over a
//! byte slice, so it is fully unit-testable with crafted frames (no VM).
//!
//! CONTRACT STUB (ADR-0018, WP-C2): signatures frozen; WP-C2 fills the bodies,
//! adds unit tests (crafted Ethernet + ARP frames), and REMOVES the `#![allow]`.
#![allow(dead_code, unused_variables, unused_imports)]

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
    todo!("WP-C2: parse Ethernet, learn src->from, resolve dst MAC -> decision")
}
