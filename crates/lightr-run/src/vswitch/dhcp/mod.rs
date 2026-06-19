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

mod parse;
mod build;
#[cfg(test)]
mod tests;

use crate::network::Subnet;
use std::collections::HashMap;
use std::net::Ipv4Addr;

// ---------------------------------------------------------------------------
// Wire-format constants
// ---------------------------------------------------------------------------

pub(crate) const ETH_HDR_LEN: usize = 14;
pub(crate) const ETHERTYPE_IPV4: u16 = 0x0800;

pub(crate) const IP_PROTO_UDP: u8 = 17;
pub(crate) const IPV4_MIN_IHL_WORDS: u8 = 5;
pub(crate) const IPV4_MIN_HDR_LEN: usize = 20;

pub(crate) const UDP_HDR_LEN: usize = 8;
pub(crate) const DHCP_CLIENT_PORT: u16 = 68;
pub(crate) const DHCP_SERVER_PORT: u16 = 67;

/// BOOTP fixed header length (op..file inclusive), before the options area.
pub(crate) const BOOTP_FIXED_LEN: usize = 236;
/// `0x63825363` — the DHCP magic cookie that precedes the options.
pub(crate) const DHCP_MAGIC_COOKIE: u32 = 0x6382_5363;

pub(crate) const BOOTREQUEST: u8 = 1;
pub(crate) const BOOTREPLY: u8 = 2;
pub(crate) const HTYPE_ETHERNET: u8 = 1;
pub(crate) const HLEN_ETHERNET: u8 = 6;

/// The BOOTP `flags` broadcast bit (RFC 2131 §2): set when the client cannot
/// yet receive unicast IP, so the reply must be broadcast.
pub(crate) const BOOTP_FLAG_BROADCAST: u16 = 0x8000;

// DHCP option codes.
pub(crate) const OPT_PAD: u8 = 0;
pub(crate) const OPT_SUBNET_MASK: u8 = 1;
pub(crate) const OPT_ROUTER: u8 = 3;
pub(crate) const OPT_DNS: u8 = 6;
pub(crate) const OPT_LEASE_TIME: u8 = 51;
pub(crate) const OPT_MESSAGE_TYPE: u8 = 53;
pub(crate) const OPT_SERVER_ID: u8 = 54;
pub(crate) const OPT_END: u8 = 255;

// DHCP message types (option 53 values).
pub(crate) const DHCP_DISCOVER: u8 = 1;
pub(crate) const DHCP_OFFER: u8 = 2;
pub(crate) const DHCP_REQUEST: u8 = 3;
pub(crate) const DHCP_ACK: u8 = 5;

/// Lease time advertised in OFFER/ACK (option 51), in seconds.
pub(crate) const LEASE_SECS: u32 = 3600;

/// Locally-administered, unicast gateway MAC for the switch's virtual port.
/// Bit 0 of the first octet = 0 (unicast); bit 1 = 1 (locally administered).
/// `udhcpc`/`dhclient` only need a stable source MAC for the offer/ack.
pub(crate) const GATEWAY_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

pub(crate) const BROADCAST_MAC: [u8; 6] = [0xff; 6];
pub(crate) const BROADCAST_IP: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);

/// Lease store: client MAC → leased IP. Pre-seeded from the registry's
/// deterministic allocation so DHCP simply hands back the assigned address.
#[derive(Default)]
pub struct LeaseStore {
    pub(crate) leases: HashMap<[u8; 6], Ipv4Addr>,
}

impl LeaseStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed the fixed lease the registry assigned to `mac`.
    pub fn insert(&mut self, mac: [u8; 6], ip: Ipv4Addr) {
        self.leases.insert(mac, ip);
    }

    /// The IP the registry assigned to `mac`, if any.
    fn get(&self, mac: &[u8; 6]) -> Option<Ipv4Addr> {
        self.leases.get(mac).copied()
    }
}

/// Parsed view of an inbound BOOTP/DHCP request we recognized.
pub(crate) struct DhcpRequest {
    /// Client MAC (BOOTP `chaddr`, first 6 bytes).
    pub(crate) chaddr: [u8; 6],
    /// Transaction id (`xid`), echoed verbatim in the reply.
    pub(crate) xid: [u8; 4],
    /// `secs` field, echoed.
    pub(crate) secs: [u8; 2],
    /// Whether the broadcast flag is set (reply must be broadcast).
    pub(crate) broadcast: bool,
    /// `giaddr` (relay) — echoed.
    pub(crate) giaddr: [u8; 4],
    /// DHCP message type from option 53.
    pub(crate) msg_type: u8,
}

/// Handle one inbound DHCP frame (full Ethernet/IP/UDP/BOOTP). Returns the
/// reply FRAME to send back on the ingress port, or `None` if not DHCP.
pub fn handle(
    frame: &[u8],
    leases: &mut LeaseStore,
    subnet: &Subnet,
    dns_ip: Ipv4Addr,
) -> Option<Vec<u8>> {
    let req = parse::parse_dhcp(frame)?;

    // Only DISCOVER/REQUEST are answered; map to the reply type.
    let reply_type = match req.msg_type {
        DHCP_DISCOVER => DHCP_OFFER,
        DHCP_REQUEST => DHCP_ACK,
        _ => return None,
    };

    // The registry owns allocation: without a pre-seeded lease we stay silent.
    let yiaddr = leases.get(&req.chaddr)?;

    Some(build::build_reply(&req, reply_type, yiaddr, subnet, dns_ip))
}
