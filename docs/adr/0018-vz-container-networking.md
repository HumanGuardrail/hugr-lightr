# ADR-0018 — Docker container networking on `vz`: a daemonless userspace L2 switch

- **Status:** Accepted (owner mandate 2026-06-19, verbatim: "go live com parity
  absoluta de features e no minimo 3 killer features que nao tem no docker").
  The load-bearing transport (`VZFileHandleNetworkDeviceAttachment` over a
  datagram socket) is **gated on a de-risk spike** — GREEN unblocks the
  integration WPs; a RED falls back to the host-relay alternative (below), and
  this status note is updated, never silently.
- **Date:** 2026-06-19

One line: container↔container reachability, name-DNS (`curl http://web`), UDP, and
`--hostname/--add-host/--dns/-P` on the `vz` engine are delivered by a **pure-Rust,
network-scoped, host-side userspace L2 switch** — each guest joins it through a
`VZFileHandleNetworkDeviceAttachment` NIC over a `socketpair(AF_UNIX, SOCK_DGRAM)`
— keeping **one microVM per container** (isolation unchanged) and **zero resident
daemon** (the switch lives and dies with its network).

## Context

F-304 is the last large Docker-parity gap. Shipped today: `-p` publish (native +
vz, WP-NET1/NET2) and compose env-discovery (WP-DISC). Missing: true
container↔container networking, name-DNS, UDP publish, foreground `-p`, and
`--hostname/--add-host/--dns/-P`. The hard part is that each `lightr run --engine
vz` is a **separate microVM**, so peers must be wired across VM boundaries.

Four facts settle the design (verified against the macOS SDK headers + the
established practice of lima / cirruslabs / Code-Hex/vz, not assumed):

1. **Apple's NAT isolates guests.** `VZNATNetworkDeviceAttachment` runs each VM
   behind a vmnet NAT; two VMs in **separate processes** on the same
   `192.168.64.x` subnet **cannot** reach each other. True guest-to-guest L2
   (`VZVmnetNetworkDeviceAttachment`) is **macOS 26+** (this box is 15.3.2) — out.
2. **Bridged NIC needs a restricted entitlement.** `VZBridgedNetworkDeviceAttachment`
   requires `com.apple.vm.networking`, which Apple must provision for the team —
   out (the owner has no Apple Developer account).
3. **The file-handle NIC needs nothing extra.** `VZFileHandleNetworkDeviceAttachment`
   ("transmits the raw packets/frames … at the data-link layer" over "a connected
   **datagram** socket") carries **no entitlement line** — only
   `com.apple.security.virtualization`, which `packaging/vz.entitlements` already
   ad-hoc-signs. This is the decisive unlock: it works on this Intel box, no Apple
   account.
4. **No host `AF_VSOCK` on macOS** — already handled by the file-channel pattern
   (`IP_FILE`/`CMD_FILE`/`EXIT_FILE`); the switch reuses the same daemonless,
   supervisor-scoped philosophy as the existing `portforward.rs`.

## Decision

1. **Userspace L2 switch (the gvproxy / pasta / podman-machine model), pure Rust.**
   A *network* is a host object. Each member VM gets a file-handle NIC whose host
   end is one half of a `socketpair(AF_UNIX, SOCK_DGRAM)` (one datagram == one
   Ethernet frame). A host-side switch owns every member socket, learns MAC→port
   from source addresses, forwards known-unicast to its port and floods
   broadcast/unknown/ARP to the rest. Container↔container traffic is real L2 **in
   the switch**, never touching the host network stack.

2. **Dual-NIC — mesh + egress.** A container on a user network keeps the existing
   `VZNATNetworkDeviceAttachment` (`eth0`, internet egress) **and** gains the
   file-handle NIC (`eth1`, the mesh). It gets both peer reachability and outbound
   internet, matching Docker. A container on **no** user network is byte-for-byte
   the path shipped today (single NAT NIC) — zero regression.

3. **Names = embedded DNS + `/etc/hosts` injection.** The switch is the
   DHCP-advertised resolver (option 6 = switch IP); it answers A records for
   container/service names + aliases from the network registry and forwards
   everything else upstream (host `/etc/resolv.conf`). As an offline-safe
   belt-and-suspenders — and the home for `--add-host` — the supervisor also
   injects `/etc/hosts` / `/etc/hostname` / `resolv.conf` into the guest rootfs
   before boot. `curl http://web` resolves and round-trips.

4. **Daemonless lifecycle.** The switch is **network-scoped**, born lazily by the
   first member's supervisor and reference-counted in the network registry (an
   `flock`-guarded dir under `$LIGHTR_HOME/net/<id>/`, mirroring the gc lock law);
   the last member to leave stops it. Nothing of ours is resident between runs —
   the A4 "`ps` proves zero" invariant holds, exactly as `vz`/`wsl` lean on the
   OS's VM without us running a daemon.

5. **Per-container VM isolation is preserved.** Only the NIC attachment changes;
   one microVM per container stays. Containers on one network share an L2
   broadcast domain — *exactly* Docker bridge semantics — but remain
   **inter-VM**, not intra-VM. (This is the explicit reason the pod-VM
   alternative was rejected; see below.)

6. **CLI surface (honest guards).** `lightr network create|ls|rm <name>`; `run
   --network <name> …`; `--hostname <h>`, `--add-host host:ip`, `--dns <ip>`,
   `-P` (auto host-port), and UDP publish (`-p H:C/udp`). `--network` requires
   `vz` + `--rootfs`; `ns`/`wsl` networking is a named future ring (Linux netns +
   veth, not testable on this Intel Mac), reported with an honest `Unsupported`,
   never a silent skip.

7. **Module decomposition = the frozen seam (implemented as a stub before fan-out).**
   - `crates/lightr-run/src/network.rs` — `NetworkId`, `Member { name, mac, ip,
     aliases, ports }`, `NetworkRegistry` (create / join / leave / list /
     refcount, `flock`), `Subnet` + deterministic IP/MAC allocation. **[WP-C1]**
   - `crates/lightr-run/src/vswitch/switch.rs` — Ethernet parse + MAC-learning
     table + forward decision; pure fn over `&[u8]`, unit-tested with no VM. **[WP-C2]**
   - `crates/lightr-run/src/vswitch/dhcp.rs` — DISCOVER/REQUEST→OFFER/ACK + lease
     store + advertise gateway/DNS; pure parse/build, unit-tested. **[WP-C3]**
   - `crates/lightr-run/src/vswitch/dns.rs` — A from registry + upstream forward;
     pure parse/build, unit-tested. **[WP-C4]**
   - `crates/lightr-run/src/vswitch/mod.rs` — `VSwitch` runtime: owns the
     per-member DGRAM sockets, the poll/forward loop wiring C2–C4, and the
     start-on-first / stop-on-zero `Drop` lifecycle. **[WP-C5]**
   - `crates/lightr-engine` — `lightr_vz_run` FFI gains an optional net-fd param;
     `VzEngine::run` creates the socketpair and passes the guest fd. **[WP-C6]**
   - `crates/lightr-engine/shim/vz.swift` — build `VZFileHandleNetworkDeviceAttachment`
     from the fd, alongside the NAT NIC (dual-NIC). **[WP-C7]**
   - `scripts/build-kernel-x86.sh` — explicitly `--enable IP_PNP IP_PNP_DHCP`
     (the script's enable-list does not list them today; the NAT path works via a
     transitive default) + extend the verify-grep (fail-closed) + re-pin the
     bzImage sha256. **[WP-C8]**
   - `crates/lightr-run/src/lib.rs` (`supervise_vz`) — join the network, take the
     switch-assigned IP **from the registry** (not `IP_FILE` polling), pass the
     net-fd into `engine.run`, point `portforward::start_to` at it, leave on
     teardown; `SpecOnDisk` gains `network/hostname/add_host/dns` (serde-default,
     back-compat). `crates/lightr-init` `InitSpec` gains the same + PID1 applies
     `sethostname`/`resolv.conf`. **[WP-C9]**
   - `spikes/s5-vz-switch/run.sh` + acceptance — two VMs: ping/curl by leased IP,
     `curl http://<name>` via embedded DNS, `-p` regression, daemonless teardown
     (`ps` clean). **[WP-C10]**

8. **Validation (Intel, this box).** The de-risk spike first proves frame
   transport (socketpair DGRAM ↔ file-handle NIC, frame-boundary fidelity, fd
   lifetime, buffer tuning). The pure-logic crates (C1–C4) are unit-tested with
   crafted Ethernet/ARP/DHCP/DNS datagrams — **no VM, CI-green**. C10 proves the
   whole stack end-to-end on the i7-9750H, no extra hardware.

## Rejected alternatives

- **A — host-relay through the NAT gateway.** Keep NAT, route container↔container
  through host-bound forwarders on `192.168.64.1`. Rejected: the host-can-bind /
  guest-can-dial-an-arbitrary-gateway-port assumption is **unproven** and flagged
  as its own top risk; TCP-only; a trust-funnel through the host; `/etc/hosts`
  staleness on peer restart. Retained **only** as the documented fallback if the
  de-risk spike comes back RED.
- **B — shared pod-VM.** One VM runs N containers on an in-guest Linux bridge +
  dnsmasq. Rejected: it **downgrades isolation** from per-container hardware VM to
  shared-kernel pod-level (a guest-kernel LPE escapes into siblings) — violating
  the "isolation à la carte" principle — and is HIGH complexity (C ABI + N-share
  shim + multi-service PID1). Noted as a possible **future opt-in** for explicit
  "share a kernel" use cases, not the default.
- **Bridged NIC** (`VZBridgedNetworkDeviceAttachment`): restricted entitlement —
  see Context (2).

## Consequences

Closes the F-304 Phase-2 set (container↔container, name-DNS, UDP,
`--hostname/--add-host/--dns/-P`) on `vz`, validated on Intel, with **per-container
VM isolation intact**, **zero resident daemon**, **no Apple account and no
restricted entitlement**. The switch is line-rate-modest (a userspace copy per
frame) — correct for a service mesh, on-brand for the userspace-forward lineage,
not a high-throughput data plane. `ns`/`wsl` networking (native netns + veth)
stays a named future ring. When macOS 26's `VZVmnetNetworkDeviceAttachment` is a
viable floor, true guest-to-guest L2 can retire the switch — a different ADR for a
26+ baseline, explicitly out of scope for an Intel-macOS-15 product today.
