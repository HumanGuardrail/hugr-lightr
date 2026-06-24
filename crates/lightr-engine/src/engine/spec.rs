//! ExecSpec — the per-run execution descriptor handed to every engine.

use std::path::Path;

// ── Mount types (R-MOUNT, parity-contract.md §0) ────────────────────────────
//
// `ExecSpec` borrows a `&[ResolvedMount]` (R-EXECSPEC). Because `lightr-run`
// depends on `lightr-engine` (NOT the reverse — see lightr-run/Cargo.toml), the
// post-resolution mount type that `ExecSpec` carries MUST live in this lower
// crate to stay acyclic. `lightr-run::run::mount` (R-MOUNT's named file)
// re-exports `MountKind` + `ResolvedMount` and owns the pre-resolution
// `MountSpec` + the parser surface. AMBIGUITY RESOLVED MINIMALLY: the contract
// names mount.rs as the type home, but the engine→run cycle forbids ExecSpec
// from referencing a lightr-run type — so the carried types are defined here and
// re-exported there; the freeze-gate still lands ONE canonical surface.

/// The five Docker volume kinds (R-MOUNT). Re-exported by `lightr_run::MountKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountKind {
    /// Content-addressed ref hydrated CoW into the run cwd (Lightr-native).
    CasRef,
    /// Host path bind-mounted into the container (`-v /host:/ctr`).
    HostBind,
    /// Docker named volume (`-v name:/ctr`).
    NamedVolume,
    /// Anonymous volume (`-v /ctr`, no source).
    AnonVolume,
    /// In-memory tmpfs (`--tmpfs /ctr`).
    Tmpfs,
}

/// A mount AFTER resolution — what [`ExecSpec`]'s mount slice carries to the
/// engine (R-MOUNT / R-EXECSPEC). WP-VOL-1 fills how a `MountSpec` resolves to
/// one of these. Re-exported by `lightr_run::ResolvedMount`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMount {
    pub kind: MountKind,
    /// Resolved host source path (CasRef hydration dir, bind host path, named
    /// volume dir). `None` for tmpfs.
    pub source: Option<String>,
    pub target: String,
    pub readonly: bool,
}

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
    /// When true, the ns engine creates a network namespace (CLONE_NEWNET) so
    /// the container gets an isolated, empty net stack (loopback only) — host
    /// interfaces/ports are invisible. native ignores it (no netns); vz already
    /// isolates via its VM. Default false = share host network (current
    /// behavior). NOT part of any memo key (runtime, like `net`/`limits`).
    pub net_isolate: bool,
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

    // ── R-EXECSPEC (parity-contract.md §0) — Docker-parity exec values. ──────
    // ExecSpec only CARRIES values to the engine; the memo key is computed
    // pre-ExecSpec in memo.rs (R-KEY) and stays there. Every construction site
    // is compile-forced to `&[]`/`None` by the freeze-gate; the WPs populate
    // them. None of these enter any memo key (runtime values).
    /// Resolved mounts to set up before exec (bind/named/anon/tmpfs/CasRef).
    pub mounts: &'a [ResolvedMount],
    /// Explicit env (`-e`/`--env-file`) injected into the child/guest.
    pub env: &'a [(String, String)],
    /// Working directory inside the container (`-w`/Dockerfile WORKDIR).
    pub workdir: Option<&'a str>,
    /// User to run as (`-u`/Dockerfile USER).
    pub user: Option<&'a str>,
    /// Container hostname (`--hostname`).
    pub hostname: Option<&'a str>,
    /// `--add-host` entries (host, ip) written to the guest `/etc/hosts`.
    pub add_host: &'a [(String, String)],
    /// `--dns` resolver addresses.
    pub dns: &'a [String],
    /// Assigned mesh IP for the dual-NIC switch (ADR-0018), if any.
    pub mesh_ip: Option<std::net::Ipv4Addr>,
}
