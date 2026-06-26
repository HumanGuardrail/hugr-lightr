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

    /// `--read-only` (WP-#92). When true, the `ns` engine remounts the container
    /// rootfs READ-ONLY after pivot_root + the `/dev` setup, so the rootfs is
    /// immutable while `/dev` + `/dev/shm` stay writable (the RO remount of `/`
    /// is NON-recursive, so the tmpfs submounts keep their RW flags). Fail-closed:
    /// a requested read-only that cannot be applied aborts the run (the ns engine
    /// returns non-zero rather than exec a writable root). Other engines ignore it
    /// (native is a host process with no rootfs to remount; vz is its own VM). NOT
    /// part of any memo key (runtime, like `limits`/`net`). Default false.
    pub read_only: bool,
    /// `--shm-size` in BYTES (WP-#92). The `ns` engine mounts a tmpfs at
    /// `/dev/shm` sized to this many bytes (`mode=1777`). `None` ⇒ a default 64 MiB
    /// `/dev/shm` (Docker's default) so the mount always exists. An EXPLICIT size
    /// that cannot be applied is fail-closed (the run aborts); the default mount is
    /// best-effort. Other engines ignore it. NOT part of any memo key (runtime).
    /// Default None.
    pub shm_size: Option<u64>,

    /// `--cap-drop` (WP-#94). Linux capability names to REMOVE from the
    /// container's set (case-insensitive, optional `CAP_` prefix; the token `ALL`
    /// drops every capability). Only the `ns` engine enforces it: as the LAST step
    /// before exec (after pivot_root + all mounts), it drops the bounding set +
    /// `capset`s permitted/effective/inheritable to the desired set. native is no
    /// sandbox (honest-errored at the handler); vz caps live inside the guest. An
    /// unknown cap name is fail-closed (the run aborts non-zero). NOT part of any
    /// memo key (runtime, like `limits`/`read_only`). Default `&[]`.
    pub cap_drop: &'a [String],
    /// `--cap-add` (WP-#94). Linux capability names to ADD back on top of the
    /// post-`cap_drop` set (same parsing rules; `ALL` ⇒ every capability). The
    /// desired set = (all caps held in the userns) − `cap_drop` + `cap_add`, so
    /// `--cap-drop ALL --cap-add NET_BIND_SERVICE` ⇒ exactly that one cap. Only the
    /// `ns` engine enforces it (see `cap_drop`). NOT part of any memo key. Default
    /// `&[]`.
    pub cap_add: &'a [String],

    /// `--init` (WP-#95). When true, the `ns` engine runs a minimal PID-1 reaper
    /// inside the new pid namespace: PID 1 forks the workload (which becomes PID 2),
    /// then `waitpid(-1)`-loops to reap orphaned zombies and propagates the
    /// workload's exit code. When false, the workload itself is PID 1 (still the
    /// real fix for the pre-#95 false-isolation bug — the workload now actually
    /// ENTERS the new pid namespace). Only the `ns` engine honors it; native is a
    /// host process (no pid namespace) and vz reaps via its own guest PID 1, so for
    /// them `--init` is a recorded-only carry-slot. NOT part of any memo key
    /// (runtime, like `read_only`/`limits`). Default false.
    pub init: bool,

    /// WP-#99 (CRI slice 1): JOIN an EXISTING network namespace instead of
    /// creating one. When `Some(path)`, the `ns` engine opens the pinned netns
    /// (a CNI-created bind-mount, e.g. `/run/netns/<id>`) and `setns(CLONE_NEWNET)`
    /// into it — BEFORE `unshare(CLONE_NEWUSER)`, while still real root in the
    /// host init userns (a child userns has no caps over the host-owned netns, so
    /// joining after the userns unshare EPERMs — THE ordering rule). It then
    /// unshares WITHOUT `CLONE_NEWNET` and SKIPS the `net_isolate` loopback path.
    /// `join_netns` and `net_isolate` are mutually exclusive (join wins). This is
    /// how a CRI container shares its pod's netns. Other engines ignore it (native
    /// has no netns; vz is its own VM). RUNTIME-ONLY — NOT part of any memo key
    /// (like `net_isolate`). Default None.
    pub join_netns: Option<&'a std::path::Path>,
    /// WP-#99 (CRI slice 1): an EXPLICIT cgroup-v2 leaf name. When `Some(name)`,
    /// the `ns` engine creates `/sys/fs/cgroup/<name>` (even when limits are
    /// unlimited, so the container is always in a known, killable cgroup) and
    /// joins it. `None` ⇒ today's behavior (a transient `lightr.<pid>` leaf,
    /// created only when a limit is set). The CRI backend supplies a deterministic
    /// name so `stop` can `cgroup.kill` the whole subtree (PID 1 + descendants).
    /// Other engines ignore it. RUNTIME-ONLY — NOT part of any memo key. Default None.
    pub cgroup_name: Option<&'a str>,
}
