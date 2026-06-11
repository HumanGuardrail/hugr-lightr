# ADR-0014 — VM states as refs: boot-never (boot-once-per-machine)

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to
  morning review). **Gated on spikes S2/S5.**
- **Date:** 2026-06-11

One line: Linux on macOS runs in microVMs that the user never watches
boot — a golden VM is minted (booted once + suspended) **per machine, in
the background, at install/first-use**, and every subsequent Linux run is
a ~100–300 ms resume; suspended states (golden and per-project warm) are
content-addressed refs (page-chunked, ADR-0009).

## Context
Apple's VZ save/restore documents same-machine/same-config restore — the
original "ship golden states cross-machine" idea is not viable on macOS
today (the SOTA review caught this); minting locally keeps the promise
("boot is never a user experience") within Apple's constraints. Cloud
(`fc`) snapshots ARE portable within CPU template (Lambda/Fly/CodeSandbox
precedent). Apple's open-source Containerization kernel removes our NIH
risk; Rosetta VMs may not support save/restore (S2 verifies) — x86 images
take the ~300 ms boot path until then.

## Decision
1. Engine `vz`: Virtualization.framework via a minimal Swift shim
   (statically linked); guest kernel derived from Apple's Containerization
   kernel; **Rust static PID1 (~1 MB)**; virtio-balloon 128–256 MB
   baseline.
2. Golden state minted in background at install/first Linux use; refreshed
   on macOS/kernel-pack updates. Per-project warm states opt-in.
3. Guest mounts the **store** read-only via virtiofs and composes its view
   inside (ADR-0013) — immutability turns the host↔guest boundary into a
   content cache (the OrbStack-bridge answer). Writes: guest upper, folded
   back as snapshots.
4. The "Linux pack" (kernel+initramfs+PID1) is a **ref**, lazily fetched —
   never bundled in the ≤10 MB binary (F-601).
5. TTL suicide for any session-scoped warm VM; idle-zero law holds.

## Consequences
Supersedes ADR-0005's "no Engine trait" (now plural engines are real —
0005 marked Superseded). User-visible boots: zero. R2 feature, spike-gated.
