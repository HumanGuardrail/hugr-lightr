//! Minimal embedded DNS responder (ADR-0018, F-304 Phase-2).
//!
//! Answers A queries for container / service names (and aliases) from the
//! network's name table; forwards everything else to the host upstream resolver
//! when one is provided. This is what makes `curl http://web` resolve. Frame-in
//! / frame-out, pure parse/build — unit-testable with captured queries (no VM).
//!
//! CONTRACT STUB (ADR-0018, WP-C4): signatures frozen; WP-C4 fills the bodies,
//! adds unit tests, and REMOVES the `#![allow]`.
#![allow(dead_code, unused_variables, unused_imports)]

use std::collections::HashMap;
use std::net::Ipv4Addr;

/// name (and each alias) → IP, built from the network registry's members.
pub type NameTable = HashMap<String, Ipv4Addr>;

/// Handle one inbound DNS query frame (full Ethernet/IP/UDP/DNS). Answers A
/// records found in `names`; otherwise forwards to `upstream` if `Some`.
/// Returns the reply FRAME, or `None` if not a query we handle.
pub fn handle(frame: &[u8], names: &NameTable, upstream: Option<Ipv4Addr>) -> Option<Vec<u8>> {
    todo!("WP-C4: parse DNS query, answer A from names or forward upstream")
}
