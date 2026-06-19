//! Network registry for vz container networking (ADR-0018, F-304 Phase-2).
//!
//! Per-network membership + deterministic addressing, persisted under
//! `$LIGHTR_HOME/net/<id>/` and `flock`-guarded (mirroring the gc lock law).
//! The registry is the source of truth the userspace L2 switch reads for its
//! MAC-learning seed, DHCP leases, and DNS name table.
//!
//! CONTRACT STUB (ADR-0018, WP-C1): the signatures + types below are frozen.
//! WP-C1 fills the bodies and REMOVES the `#![allow]` line.
#![allow(dead_code, unused_variables, unused_imports)]

use std::net::Ipv4Addr;
use std::path::Path;

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

/// On-disk, `flock`-guarded network registry under `$LIGHTR_HOME/net/<id>/`.
pub struct NetworkRegistry {
    // WP-C1: home root, id, subnet, lock handle, members file path, …
}

impl NetworkRegistry {
    /// Create (or open if present) the named network, allocating its subnet.
    pub fn create(home: &Path, id: &NetworkId) -> std::io::Result<Self> {
        todo!("WP-C1: create $LIGHTR_HOME/net/<id>/ + allocate subnet, flock")
    }

    /// Open an existing network; error if absent.
    pub fn open(home: &Path, id: &NetworkId) -> std::io::Result<Self> {
        todo!("WP-C1: open existing network dir")
    }

    /// Join: allocate a deterministic MAC + IP for `name`, persist the member,
    /// bump the refcount, and return the assigned `Member`.
    pub fn join(
        &self,
        name: &str,
        aliases: &[String],
        ports: &[(u16, u16)],
    ) -> std::io::Result<Member> {
        todo!("WP-C1: deterministic MAC/IP alloc + persist + refcount++")
    }

    /// Leave: remove `name`, decrement the refcount, return remaining count.
    pub fn leave(&self, name: &str) -> std::io::Result<usize> {
        todo!("WP-C1: remove member + refcount-- ; returns remaining members")
    }

    /// All current members (the switch's flooding set + DNS/lease seed).
    pub fn members(&self) -> std::io::Result<Vec<Member>> {
        todo!("WP-C1: read members file under flock")
    }

    /// This network's subnet.
    pub fn subnet(&self) -> Subnet {
        todo!("WP-C1: return the allocated subnet")
    }

    /// List every network registered under `home`.
    pub fn list(home: &Path) -> std::io::Result<Vec<NetworkId>> {
        todo!("WP-C1: enumerate $LIGHTR_HOME/net/*")
    }
}
