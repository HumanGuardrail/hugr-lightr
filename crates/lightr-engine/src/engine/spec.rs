//! ExecSpec — the per-run execution descriptor handed to every engine.

use std::path::Path;

// ── ExecSpec ──────────────────────────────────────────────────────────────────

pub struct ExecSpec<'a> {
    pub cwd: &'a Path,
    pub command: &'a [String],
    /// ns/vz: CoW-materialized tree to pivot/boot into. Native: must be None.
    pub rootfs: Option<&'a Path>,
    /// F-203 resource caps (build-spec-parity.md §A0.4). `Copy`; default =
    /// unlimited. Applied per engine: native/ns via `crate::limits`, vz via the
    /// VM config. NOT part of the memo key.
    pub limits: lightr_core::ResourceLimits,
    /// Container networking (WP-NET2). When true, the vz engine attaches a NAT
    /// NIC + `ip=dhcp` (via `LIGHTR_VZ_NET`) and tells the guest PID1 to publish
    /// its IP (`InitSpec::net`), so the host can forward published ports to the
    /// guest. Other engines ignore it (native/ns/wsl don't VM-network here). NOT
    /// part of any memo key (runtime, like `limits`/`ports`). Default false.
    pub net: bool,
    /// ADR-0018 dual-NIC mesh (WP-C6/C7). The GUEST-side fd of a
    /// `socketpair(AF_UNIX, SOCK_DGRAM)` whose host end is owned by the userspace
    /// L2 switch (a later WP creates the pair and owns the host end). When
    /// `Some(fd)`, the vz engine attaches a SECOND virtio-net NIC — a
    /// `VZFileHandleNetworkDeviceAttachment` over this fd (`eth1`, the mesh) —
    /// ALONGSIDE the existing NAT NIC (`eth0`, egress). When `None`, behavior is
    /// byte-for-byte the single-NAT-NIC path shipped today (zero regression).
    /// Other engines ignore it (only vz attaches a file-handle NIC). NOT part of
    /// any memo key (runtime, like `limits`/`net`). Default None.
    pub net_fd: Option<std::os::raw::c_int>,
    /// ADR-0018: the per-member MAC the mesh NIC (`eth1`) must use. The network
    /// registry assigns it; the guest emits it, so the userspace switch's DHCP
    /// lease, MAC-learning, and DNS all key on the SAME MAC. `None` ⇒ the vz shim
    /// falls back to a pinned MAC (de-risk / single-guest path). Only meaningful
    /// alongside `net_fd = Some`. NOT part of any memo key (runtime).
    pub net_mac: Option<[u8; 6]>,
}
