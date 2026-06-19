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
#![allow(dead_code, unused_variables, unused_imports)]

use crate::network::Subnet;
use std::collections::HashMap;
use std::net::Ipv4Addr;

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
        todo!("WP-C3: record the deterministic lease")
    }
}

/// Handle one inbound DHCP frame (full Ethernet/IP/UDP/BOOTP). Returns the
/// reply FRAME to send back on the ingress port, or `None` if not DHCP.
pub fn handle(
    frame: &[u8],
    leases: &mut LeaseStore,
    subnet: &Subnet,
    dns_ip: Ipv4Addr,
) -> Option<Vec<u8>> {
    todo!("WP-C3: parse BOOTP, build OFFER/ACK with lease + gateway + DNS")
}
