# HuGR Lightr — Product Whitepaper (v1)

**Status: design phase. Nothing here is shipped; tense is kept honest throughout.**
Canonical vision — source of truth for this repo. 2026-06-11.

## Abstract

Docker is three products glued together — distribution (images), isolation
(namespaces, a VM on macOS), and lifecycle (a daemon) — and the glue is why it
is heavy on a laptop and expensive in a fleet. HuGR Lightr unbundles them: a
single static binary, no daemon, no images. Workspaces materialize from
CoreLink's content-addressed store in seconds; execution runs under the
lightest isolation the context permits (native on a dev Mac, microVMs in a
hostile cloud); and anything already computed never runs again, because every
execution is memoized through the Action Cache. The isolation primitives are
commodity. The substrate that makes Lightr economically different — instant
pulls, chunk-level dedup, memoized execution — is CoreLink, and CoreLink is
already in production.

## 1. The problem is not the container

**Local (macOS).** Containers are a Linux-kernel feature. Docker Desktop
therefore keeps a full Linux VM alive 24/7 — 2–4 GB of idle RAM, constant CPU,
slow VirtioFS file sharing — plus dockerd and an Electron shell. The tax is
structural: the VM is always on whether or not anything runs.

**Cloud.** A running container on Linux is a native process; there is no
runtime CPU to recover. The money leaks elsewhere: slow image pulls →
over-provisioned instances → fleets kept warm because cold starts are
expensive → poor bin-packing. Layer-granular images make every one of those
worse: change one byte, re-ship the tarball.

Both pains share a root cause: Docker's *distribution and lifecycle model*,
not its isolation. Any product that only ships "a lighter VM" or "a faster
runtime" treats the symptom.

## 2. The insight: unbundle, and anchor on the cache

Split Docker into its three glued products and ask what HuGR already owns:

| Docker bundles | Lightr's answer | Status |
|---|---|---|
| Distribution: images, layers, registry | CoreLink CAS manifests — FastCDC ~1 MiB chunks, BLAKE3, dedup, lazy hydration | **live in production** (`corelink-server`); client shipped (`clw`) |
| Isolation: namespaces/cgroups, VM on Mac | À la carte engines: `native` / `ns` / `vz` / `fc` / `docker`-compat | `Engine` seam shipped in `corelink-runners`; designed for this swap |
| Lifecycle: dockerd, 24/7 | Deleted. One static binary; nothing runs when nothing runs | design |

The value split is asymmetric: isolation is commodity open source (~5% of the
value); the content-addressed substrate underneath — and the memoization it
enables — is the differentiated ~95%, and it already exists.

## 3. The product

```
$ brew install hugr-lightr         # bin: lightr — no daemon, no account
$ lightr run @hugr/web -- pnpm dev
⚡ hydrated 1.2 GB in 3.1s (94% local cache)
▶ running native — 0 MB overhead
```

One verb carries the thesis. `lightr run <ref> -- <cmd>`:

1. Resolve the ref → root manifest digest (Action Cache).
2. **Memo check** — BLAKE3(inputs ⊕ cmd ⊕ env ⊕ toolchain). Hit → return the
   stored result in milliseconds. *The cheapest run is the one that never
   happens.*
3. Miss → hydrate, sparse and cache-first (local L1 `~/.clw/cache`, then CAS).
4. Execute under the context's isolation tier.
5. Memoize on success; attest what ran. Fail closed throughout (pinned inputs
   verified before spawn; no partial results; explicit errors over silent
   cold runs — discipline inherited from the runners core).

### Isolation tiers

| Engine | Context | Overhead | Cold start |
|---|---|---|---|
| `native` | trusted local dev (reproducibility, **not** a sandbox — stated loudly) | zero | ms |
| `ns` | trusted Linux, single-tenant (crun-level namespaces) | ~0 | ~20–50 ms |
| `vz` | macOS needing Linux or a boundary (Virtualization.framework, ephemeral) | ~5 MB | <1 s, dies with the job |
| `fc` | hostile multi-tenant cloud (Firecracker) | ~5 MB | ~125 ms; ~5 ms from snapshot |
| `docker` | compatibility/migration | dockerd | seconds |

## 4. Architecture (vision)

### 4.1 Where Lightr sits

```
HuGR (the company / brand)
 └─ CoreLink (the platform)
     ├─ Cache       — content-addressed CAS + Action Cache     (live)
     ├─ Runners     — ephemeral compute on the cache           (campaign #1)
     ├─ Workspaces  — workspace-as-object (clw client shipped) (campaign #2)
     └─ Lightr        — the runtime front door, local-first      (THIS REPO)
   hugit (the forge for agent fleets)                          (campaign #3)
```

Lightr is the local-first front door of the same fabric Runners sells: locally
it runs standalone and free; in the cloud, Lightr is what a runner lease
executes. Distribution is the clw pipeline; the registry is CoreLink's CAS/AC
over HTTPS, tenant-namespaced, PAT-authed. Lightr is a **pure client** — zero
server changes, exactly like clw.

### 4.2 Lazy rootfs

Don't pull before boot; mount a filesystem that faults chunks in from
CoreLink on first access (FUSE locally; virtio-backed chunk store inside
microVMs). Most workloads touch a small fraction of their rootfs; lazy
loading turns "pull 1.2 GB" into "fault in the ~80 MB actually read". This is
the architecture AWS published for Lambda's container loading, and what
Nydus/eStargz/SOCI approximate for OCI — but those remain layer-bound and
have no memoizing Action Cache above them.

### 4.3 Snapshot/resume

Firecracker restores a booted VM in ~5 ms. A pool of generic per-toolchain
snapshots in CAS turns "start an instance" into "restore + attach manifest".
Combined with memoization, scale-to-zero becomes the default posture.

## 5. Economics: where the 65–80% actually comes from

Not from making a process faster — from never paying for waste:

- **Memoization** — identical work returns from the AC without instantiating
  anything. For build/CI/agent workloads, repeat rates are high by nature.
- **Scale-to-zero viable** — ~125 ms cold (~5 ms from snapshot) removes the
  reason warm fleets exist.
- **Dense bin-packing** — ~5 MB/microVM overhead and no daemon per host.
- **Chunk dedup** — shared toolchains/deps stored once per tenant (GA) —
  designed to be once *globally* when cross-tenant dedup lands (staged,
  `CAP-DEDUP-CROSS-TENANT`).

| | Docker Desktop (Mac) | Lightr local | Docker cloud | Lightr cloud |
|---|---|---|---|---|
| Idle RAM | 2–4 GB | 0 | dockerd + image | ~5 MB/microVM |
| "Pull" | whole layers | dedup chunks, lazy | layers | lazy, only what's touched |
| Cold start | seconds | ms (native) | 1–5 s | ~125 ms (~5 ms snapshot) |
| Cache hit | runs again | **does not run** | runs again | **does not run** |

## 6. The funnel (the business reason Lightr exists)

Stage 0→1: a Mac dev installs the free binary; no account, no server touched;
COGS ≈ 0. Stage 1→2: after a week they hold dozens of content-addressed
snapshots; wanting them on a second machine or a teammate's is a flag-flip
into a CoreLink tenant — the upgrade is not buying a feature. Stage 2→3: once
the team's workspaces live in the CAS, running CI or agent sandboxes anywhere
else means paying to download what already sits in CoreLink — the Runners and
Workspaces pitches write themselves. Lightr carries no bill of its own; it is
the CAC engine and cache-warmer for the platform. (Funnel detail and metrics:
`docs/VISION.md`.)

## 7. The moat, stated honestly

A registry can be cloned; a warm, memoizing, content-addressed substrate with
production tenancy is the accumulated part. Every snapshot a tenant pushes
makes their own cache hit-rate higher (lock-in by usefulness, not contract).
The compounding network effect — every new user warming a shared public-deps
cache — **depends on cross-tenant dedup, which is designed-in but staged**;
we state that dependency rather than claim it live. Modal, Fly and Depot each
built closed versions of this machinery for internal use; Lightr is that
machinery as a product, anchored on a cache that already bills customers.

## 8. Positioning

- **vs Docker Desktop** — the incumbent pain: always-on VM, layer pulls, a
  daemon. Lightr: nothing idle, chunk-lazy, daemonless.
- **vs OrbStack / Apple `container`** — excellent, *lighter VMs*. Same
  category as Docker; they validate the `vz` engine choice but keep the
  image/registry model. Lightr competes on a different axis: distribution +
  memoization. Devs will still compare day one — the local product must win
  standalone.
- **vs Podman** — daemonless, yes; image-bound and layer-bound, still.
- **vs Nydus/eStargz/SOCI** — lazy loading inside the OCI/layer model; no
  content-defined chunking across artifacts, no Action Cache above.
- **vs Modal/Fly/Depot** — proof the architecture works; none sells it as a
  local-first product on an open cache platform.

## 9. Principles (decided — do not relitigate without the owner)

1. **No daemon, ever.** Nothing runs when nothing runs; `ps` proves it.
2. **No images.** Manifests + chunks; lazy by default. OCI is an import
   format, not the model.
3. **Free local, forever, no account.** Stage 1 touches no servers. The
   funnel dies the day the first `lightr run` needs a login.
4. **Isolation à la carte.** `native` is reproducibility, not a sandbox —
   said loudly in docs and CLI output. Hostile tenancy gets hardware
   boundaries (`fc`), never best-effort namespaces.
5. **Memoize-first.** The AC check precedes any provisioning, always.
6. **Pure client of CoreLink.** Zero server changes; tenancy, auth and dedup
   semantics are CoreLink's law, including its tense (intra-tenant at GA,
   cross-tenant staged).
7. **Fail closed.** Pinned inputs verified before spawn; no partial results;
   explicit errors over silent cold runs.
8. **Sequencing: after Runners M1.** Demand the free tier creates must have
   somewhere to convert.
9. **Never charge for the customer's own compute twice** — inherited
   platform-wide: a memoized result is never billed as a run.

## 10. Roadmap

- **v0.1 — feel the thesis (local).** `lightr run` native engine, clw pipeline,
  local-only cache, macOS arm64. One sprint. (`docs/MVP-v0.1.md`.)
- **v0.2 — the boundary.** `vz` ephemeral microVMs; OCI image import;
  Linux `ns` engine.
- **v0.3 — Stage 2.** HuGR account → CoreLink tenant; shared cache;
  team refs.
- **v1.x — the cloud half.** `fc` engine, lazy rootfs, snapshot pool;
  Lightr as the runtime a Runners lease executes.

## 11. Risks & non-goals

- **OrbStack comparison on day one** — if Lightr local is not spectacular
  standalone, the funnel never starts. This is the existential product risk.
- **Ecosystem gravity** (docker-compose, OCI tooling, muscle memory) — the
  `docker` compat engine and OCI import are the migration ramp, not optional.
- **Cross-tenant dedup is staged** — the global network effect waits on it;
  unit economics at GA rest on intra-tenant dedup + memoization alone.
- **Non-goals:** not a Kubernetes (orchestration is Runners' fabric); not a
  security product at v0.1 (`native` says so on the tin); no OCI-replacement
  crusade.

## 12. Why HuGR wins

Two-thirds of Lightr predates Lightr, in production or shipped: the cache with
real tenants and ~80%-margin unit economics, the clw snapshot/hydrate/memoize
pipeline, and a fail-closed execution core whose `Engine` seam was explicitly
designed for the microVM swap. The remaining third — the engines and the
local UX — is the commodity part. Competitors would have to build the
differentiated part; HuGR only has to build the easy part.
