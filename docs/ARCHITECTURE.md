# Architecture — how Lightr runs things

> **Superseded for the perf core:** whitepaper v2 §3–§5 + ADRs 0009–0016
> govern the store/views/engines now (CoW, O(1) views, boot-never). This
> file remains for the funnel-era seams narrative.

## Design principles

1. **No daemon.** One static binary. Nothing runs when nothing runs.
2. **No images.** A "pull" is a sparse, lazy hydration of a CAS manifest —
   only the chunks the workload actually touches, deduplicated at ~1 MiB
   FastCDC granularity. Docker layers re-download a whole tarball when one
   byte changes; chunks don't.
3. **Isolation à la carte.** The isolation tier is a property of the
   *context*, not of the runtime. Trusted local dev needs reproducibility,
   not a sandbox; hostile multi-tenant cloud needs a hardware boundary.
4. **Never run what is already known.** Every execution is memoized through
   the Action Cache (`clw run` semantics). A cache hit returns the stored
   result in milliseconds with zero instantiation.
5. **Fail closed.** Inherited from runners: pinned inputs verified before
   spawn, no partial results stored, explicit errors over silent cold runs.

## Execution flow

```
lightr run @tenant/workspace -- <cmd>
  │
  1. Resolve ref → RefRecord → root manifest digest        (AC)
  2. Memo check: BLAKE3(inputs ⊕ cmd ⊕ env ⊕ toolchain)    (AC)
  │    HIT  → return stored result. Done. Nothing ran.
  │    MISS ↓
  3. Hydrate, sparse + cache-first                          (CAS + ~/.clw/cache)
  │    only manifest entries in scope; local L1 first;
  │    lazy fault-in for the rest (see "Lazy rootfs")
  4. Execute under the context's isolation tier             (Engine)
  5. Memoize result (success only), attest what ran         (AC)
```

## Isolation tiers (the `Engine` implementations)

The seam is `corelink-runner/src/isolation.rs`'s `Engine` trait — spawn /
probe / exec / teardown against a `ContainerSpec`. Lightr promotes it to the
public interface and ships multiple engines:

| Engine | Context | Overhead | Cold start |
|---|---|---|---|
| `native` | trusted dev workload, local | zero | ms |
| `ns` (namespaces/cgroups, crun-level) | trusted Linux, single-tenant | ~0 | ~20–50 ms |
| `vz` (Virtualization.framework microVM) | macOS needing Linux or a sandbox | ~5 MB | <1 s, ephemeral — dies with the job |
| `fc` (Firecracker microVM) | cloud, hostile multi-tenant | ~5 MB | ~125 ms (~5 ms from snapshot) |
| `docker` (legacy) | compatibility/transition | dockerd | seconds |

Precedents worth studying: OrbStack and Apple's `container` CLI
(Virtualization.framework, per-container lightweight VMs) for `vz`;
AWS Lambda's Firecracker fleet for `fc`.

## Lazy rootfs (cloud)

Instead of pulling an image before boot, mount a filesystem whose reads
fault in chunks from CoreLink on first access (FUSE locally; virtio-blk /
virtiofs backed by a chunk store inside the microVM). This is the
architecture AWS published for Lambda ("on-demand container loading") and
what Nydus/eStargz/SOCI approximate for OCI — but anchored on a CAS that
already exists (dedup intra-tenant at GA; cross-tenant staged). Most workloads touch a small
fraction of their rootfs; lazy loading converts "pull 1.2 GB" into "fault
in the ~80 MB actually read".

## Snapshot/resume (cloud)

Firecracker snapshots restore a booted VM in ~5 ms. Keep a pool of generic
snapshots (per toolchain profile) in CAS; "starting an instance" becomes
restoring a snapshot + attaching the workspace manifest. Combined with
memoization this makes scale-to-zero the default posture, which is where
the 65–80% cloud cost reduction actually comes from: no warm fleets, dense
bin-packing, dedup across tenants.

## Seams with the existing repos

```
lightr (this repo)
 ├── distribution  = clw crates (snapshot / hydrate / run, local L1 cache)
 ├── registry      = CoreLink CAS/AC over HTTPS (tenant-namespaced, PAT auth)
 └── execution     = Engine trait lineage from corelink-runners
```

- **corelink-server**: untouched. Lightr is a pure client of `/v1/cas` and
  `/v1/ac`, exactly like clw. Stage-1 (free local) never calls it.
- **corelink-workspaces**: the house pattern is wire-level seams with
  transcribed types proven by shared conformance vectors (no git/path
  deps between repos). Whether Lightr vendors the clw crates, depends on
  them, or transcribes the contract is an open decision — see MVP doc.
- **corelink-runners**: shares the `Engine` contract and the fail-closed
  lifecycle (lease → spec → verify → spawn → probe → exec → teardown →
  forensic sweep). Runners is the multi-tenant *fabric*; Lightr is the
  *runtime* — locally it runs standalone, in the cloud it is what a
  runner lease executes.

## What Lightr is not

- Not an OCI replacement crusade: a compat engine (`docker`) and an
  OCI-image import path keep migration cheap.
- Not a Kubernetes: no cluster orchestration; the fabric/scheduling story
  belongs to Runners.
- Not a security product on day one: `native` mode is explicitly "no
  isolation, reproducibility only" — said out loud in the docs, so nobody
  mistakes Stage 1 for a sandbox.
