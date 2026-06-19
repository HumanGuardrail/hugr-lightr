//! Deterministic allocation helpers: subnet and MAC address derivation.

use super::types::{MacAddr, NetworkId, Subnet};
use std::net::Ipv4Addr;

// ─────────────────── deterministic allocation helpers ──────────────────────

/// Stable subnet third-octet `k` from the network id: `10.69.<k>.0/24`.
/// `k` is `blake3(id)[0]`, so distinct ids land on distinct /24s with high
/// probability and the same id is always the same subnet across processes.
pub(super) fn subnet_for(id: &NetworkId) -> Subnet {
    let h = blake3::hash(id.as_bytes());
    let k = h.as_bytes()[0];
    Subnet {
        base: Ipv4Addr::new(10, 69, k, 0),
        prefix: 24,
        gateway: Ipv4Addr::new(10, 69, k, 1),
    }
}

/// Deterministic locally-administered unicast MAC for `name`:
/// `0a:00:00` + 3 bytes of `blake3(name)`. `0x0a` has the locally-administered
/// bit set and the unicast (group) bit clear, so it never collides with a
/// real vendor OUI and is a valid source/dest MAC.
pub(super) fn mac_for(name: &str) -> MacAddr {
    let h = blake3::hash(name.as_bytes());
    let b = h.as_bytes();
    MacAddr([0x0a, 0x00, 0x00, b[0], b[1], b[2]])
}
