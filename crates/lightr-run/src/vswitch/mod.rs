//! Daemonless, network-scoped userspace L2 switch (ADR-0018, F-304 Phase-2).
//!
//! Owns one `AF_UNIX`/`SOCK_DGRAM` socket per member (the host end of each
//! guest's `VZFileHandleNetworkDeviceAttachment` — de-risk spike S5-FHNET
//! proved one datagram == one Ethernet frame). A poll/forward thread runs the
//! [`switch`] (MAC learning + flood), and intercepts DHCP/DNS frames destined
//! for the gateway, answering via [`dhcp`] / [`dns`]. The switch is born by the
//! first member's supervisor and stops when the last member leaves (the
//! registry refcount arbitrates) — nothing of ours is resident between runs.
//!
//! CONTRACT STUB (ADR-0018, WP-C5): submodules [`switch`]/[`dhcp`]/[`dns`] are
//! filled by WP-C2/C3/C4; this runtime is filled by WP-C5 (which removes the
//! `#![allow]`). The de-risk spike (commit 7637d3e, `spikes/s5-vz-fhnet/`)
//! documents the socket/buffer/fd-lifetime findings WP-C5 must honor (read with
//! a ≥64 KiB buffer — VZ can hand >1514 B GSO aggregates; SO_RCVBUF ≥ 2×SNDBUF).
#![allow(dead_code, unused_variables, unused_imports)]

pub mod dhcp;
pub mod dns;
pub mod switch;

use crate::network::{NetworkId, Subnet};
use std::net::Ipv4Addr;
use std::os::unix::io::RawFd;

/// A running switch instance for one network.
pub struct VSwitch {
    // WP-C5: member sockets (UnixDatagram), MacTable, LeaseStore, NameTable,
    // subnet, stop flag / thread join handle.
}

impl VSwitch {
    /// Start a switch for `id` on `subnet` (no members yet); spawns the
    /// poll/forward thread. The registry refcount makes this effectively
    /// once-per-network.
    pub fn start(id: &NetworkId, subnet: Subnet) -> std::io::Result<Self> {
        todo!("WP-C5: bind nothing yet, spawn the poll/forward thread")
    }

    /// Add a member: take ownership of the host end (`host_fd`) of its
    /// socketpair, registered with its assigned MAC/IP/name for switching +
    /// DHCP + DNS.
    pub fn add_member(
        &self,
        host_fd: RawFd,
        mac: [u8; 6],
        ip: Ipv4Addr,
        name: &str,
    ) -> std::io::Result<()> {
        todo!("WP-C5: wrap fd as UnixDatagram, register in switch/lease/name tables")
    }

    /// Remove a member by name (its socket is dropped, carrier drops in-guest).
    pub fn remove_member(&self, name: &str) -> std::io::Result<()> {
        todo!("WP-C5: drop the member socket + table entries")
    }

    /// Stop the switch: drop all sockets and join the thread.
    pub fn shutdown(self) -> std::io::Result<()> {
        todo!("WP-C5: signal stop, join the poll thread")
    }
}
