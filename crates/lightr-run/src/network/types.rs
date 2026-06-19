//! Network types: identifiers, addresses, membership, subnets, and on-disk serde shapes.
//!
//! The public `Member`/`Subnet`/`MacAddr` types are frozen (ADR-0018) and do
//! NOT derive serde, so we persist via small mirror records. This also keeps
//! the on-disk format explicit and stable independent of the in-memory types.

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// A network's user-facing name (e.g. `"web-net"`); also its on-disk dir name.
pub type NetworkId = String;

/// A 6-byte Ethernet MAC address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MacAddr(pub [u8; 6]);

/// One member of a network (a single container).
#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub aliases: Vec<String>,
    pub mac: MacAddr,
    pub ip: Ipv4Addr,
    /// `(host, container)` published ports for this member.
    pub ports: Vec<(u16, u16)>,
}

/// The IPv4 subnet a network leases addresses from (e.g. `10.69.0.0/24`,
/// gateway `.1` = the switch's virtual IP, which is also the DNS server).
#[derive(Clone, Copy, Debug)]
pub struct Subnet {
    pub base: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Ipv4Addr,
}

// ─────────────────────── on-disk serde shapes ──────────────────────────────

#[derive(Serialize, Deserialize)]
pub(super) struct SubnetOnDisk {
    pub base: [u8; 4],
    pub prefix: u8,
    pub gateway: [u8; 4],
}

impl From<Subnet> for SubnetOnDisk {
    fn from(s: Subnet) -> Self {
        SubnetOnDisk {
            base: s.base.octets(),
            prefix: s.prefix,
            gateway: s.gateway.octets(),
        }
    }
}

impl From<SubnetOnDisk> for Subnet {
    fn from(s: SubnetOnDisk) -> Self {
        Subnet {
            base: Ipv4Addr::from(s.base),
            prefix: s.prefix,
            gateway: Ipv4Addr::from(s.gateway),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct MemberOnDisk {
    pub name: String,
    pub aliases: Vec<String>,
    pub mac: [u8; 6],
    pub ip: [u8; 4],
    pub ports: Vec<(u16, u16)>,
}

impl From<&Member> for MemberOnDisk {
    fn from(m: &Member) -> Self {
        MemberOnDisk {
            name: m.name.clone(),
            aliases: m.aliases.clone(),
            mac: m.mac.0,
            ip: m.ip.octets(),
            ports: m.ports.clone(),
        }
    }
}

impl From<MemberOnDisk> for Member {
    fn from(m: MemberOnDisk) -> Self {
        Member {
            name: m.name,
            aliases: m.aliases,
            mac: MacAddr(m.mac),
            ip: Ipv4Addr::from(m.ip),
            ports: m.ports,
        }
    }
}
