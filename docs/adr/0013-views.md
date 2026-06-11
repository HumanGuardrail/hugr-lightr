# ADR-0013 — Views: O(1) materialization + the solidifier

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to
  morning review). **Gated on spikes S1/S3** for the mount layer; R0 ships
  the CoW-clone path (F-103 R0 form).
- **Date:** 2026-06-11

One line: `hydrate` mounts a **view** of a manifest (appears in O(1),
lazy-faults content from the store) and a background **solidifier**
promotes hot files to native CoW clones, after which the mount evaporates —
first access instant, steady state native; `run` starts before data
arrives.

## Context
CoW cloning is O(files); monorepos deserve O(1). Proven organs: EdenFS
(NFS-loopback on macOS, redirections ≈ solidifier), GVFS/VFS-for-Git
(300k+ files), composefs/EROFS (kernel-native views on Linux, podman
adoption). FSKit (macOS 15+) is promising but young.

## Decision
1. Linux views: **composefs** (EROFS metadata over the store + overlay
   upper for writes) — kernel-speed, no FUSE, pairs with fs-verity.
2. macOS views: **NFS-loopback** in-process server (EdenFS-proven);
   **FSKit** adopted when S1-class testing says it's ready.
3. Solidifier: background promote-on-access (and predictive: manifest
   order, prior run's read-set); when fully solid, unmount — zero
   steady-state indirection.
4. Write-back: upper-layer writes fold into snapshots through the index.
5. The view layer is optional per invocation (`--solid` forces full CoW
   clone first) and per platform (microwave clause: copy rung still works).

## Consequences
Materialization decouples from tree size; the FS layer becomes the
dependency oracle for deep-memo (ADR-0016). R2 feature; R0's CoW path is
the fallback forever.
