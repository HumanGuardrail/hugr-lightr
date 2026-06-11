# Vision — why Cell exists

## The two pains

**Local (macOS).** Containers are a Linux-kernel feature; macOS cannot run
them natively, so Docker Desktop keeps a full Linux VM alive 24/7 — 2–4 GB
of idle RAM, constant CPU, slow VirtioFS file sharing, plus dockerd and an
Electron GUI. The villain is not the container; it is the always-on VM.

**Cloud.** A running container on Linux is a native process — there is no
65% of runtime CPU to recover. The money leaks structurally: slow image
pulls → over-provisioned instances → fleets kept warm because cold starts
are expensive → poor bin-packing. The cure is not a faster process; it is
**scale-to-zero made viable + dense bin-packing + dedup**.

## What HuGR already has

Cell is the missing third of a system that is two-thirds built:

- **CoreLink** (`corelink-server`) — production content-addressed store:
  FastCDC ~1 MiB chunks, BLAKE3, dedup, multi-region, Action Cache.
- **clw** (`corelink-workspaces`) — snapshot/hydrate of whole directories
  via CAS with a local L1 cache, and memoized command execution
  (`clw run`). Deliberately zero isolation.
- **corelink-runners** — leased, isolated execution. Docker-per-job today,
  but behind an `Engine` trait (`corelink-runner/src/isolation.rs`)
  explicitly designed for the swap to Firecracker. Docker there is a
  declared placeholder.

Cell = that `Engine`, promoted from internal seam to public product, with
clw as the distribution layer and CoreLink as the registry.

## The product in one line

`cell run <workspace-ref> -- <cmd>` → resolve the manifest in CAS → sparse,
lazy hydrate → execute under the right isolation for the context (native on
dev Macs, namespaces on trusted Linux, Firecracker for multi-tenant) →
memoize the result. The cheapest cell is the one that never runs: an Action
Cache hit returns in milliseconds without instantiating anything.

## What we can honestly promise

| | Docker Desktop (Mac) | Cell local | Docker cloud | Cell cloud |
|---|---|---|---|---|
| Idle RAM | 2–4 GB | 0 | dockerd + image | ~5 MB/microVM |
| "Pull" | whole layers | dedup chunks, lazy | layers | lazy, only what's touched |
| Cold start | seconds | ms (native) | 1–5 s | ~125 ms (~5 ms w/ snapshot) |
| Cache hit | runs again | **does not run** | runs again | **does not run** |

Honesty clause: Docker is not garbage on a Linux server — there it is thin.
It is garbage **on the Mac** and **as a distribution/economic model**. The
defensible advantage is not "a faster container"; it is that nobody else
ships a production CAS with cross-tenant dedup to anchor this. Modal, Fly
and Depot built closed versions of this machinery for internal use; Cell is
that machinery as a product.

## The funnel

```
                  ┌─────────────────────────────────────────────┐
  ACQUISITION     │  STAGE 0 — the pain                         │
                  │  Dev on a Mac, Docker Desktop eating 4 GB,  │
                  │  fans spinning. `brew install hugr-cell`    │
                  │  fixes it in one command.                   │
                  └──────────────────┬──────────────────────────┘
                                     │ free, no signup
                  ┌──────────────────▼──────────────────────────┐
  ACTIVATION      │  STAGE 1 — cell local (FREE, solo)          │
                  │  hydrate + native/ephemeral-microVM run.    │
                  │  Cache 100% local (~/.clw/cache).           │
                  │  Works offline. Zero servers touched.       │
                  └──────────────────┬──────────────────────────┘
                                     │ trigger: "I want this on my
                                     │ other machine / my teammate's"
                  ┌──────────────────▼──────────────────────────┐
  CONVERSION 1    │  STAGE 2 — shared cache (PAID, $)           │
                  │  HuGR login → CoreLink tenant. A snapshot   │
                  │  a teammate hydrates in seconds. Their CI's │
                  │  cache hit = your build that never runs.    │
                  └──────────────────┬──────────────────────────┘
                                     │ trigger: "the cache is already
                                     │ in the cloud — why does my CI /
                                     │ agent run somewhere else?"
                  ┌──────────────────▼──────────────────────────┐
  CONVERSION 2    │  STAGE 3 — compute (PAID, $$$)              │
                  │  Runners: flat-price, cache-warm CI.        │
                  │  Workspaces: agent sandboxes / dev boxes    │
                  │  booting in ~125 ms from the same CAS.      │
                  └─────────────────────────────────────────────┘
```

**Why each stage pushes to the next.** Stage 1→2: after a week, the dev has
dozens of content-addressed snapshots; the upgrade is not buying a feature,
it is flipping a flag on something they already have. Stage 2→3: once the
team's workspace lives in the CAS, running CI elsewhere means paying by the
minute for a cold machine to download what already sits in your CoreLink —
the Runners pitch writes itself, and the same argument covers agent
sandboxes (Workspaces).

**Economics.** Stage 1 touches no servers — COGS ≈ 0. On conversion, FastCDC
dedup does the work: the thousandth user uploading the same `node_modules`
costs a HEAD request, not storage. CoreLink already documents ~80% margin on
the Solo tier, and margin improves with scale (fuller cache = more dedup =
lower COGS per tenant). Stage 3 anchors LTV on the same trick: a cache hit
is a job that consumes no core-second. The compounding moat: every new user
warms the public-deps cache, which makes the product faster for everyone,
which attracts more users — Docker Hub's network effect, but with per-chunk
instead of per-layer economics.

## Funnel health metrics

| Stage | North-star metric | Healthy signal |
|---|---|---|
| 0→1 | installs / week | organic growth ("I killed Docker Desktop" posts) |
| 1 | snapshots / dev / week | ≥3 = habit formed |
| 1→2 | % adding a 2nd device or inviting a teammate | THE conversion trigger |
| 2 | tenant cache-hit rate | >60% = real lock-in (leaving = losing the cache) |
| 2→3 | tenants asking for CI/sandboxes | Runners pipeline |

## Honesties

1. **Stage 0→1 only works if the local product is spectacular standalone.**
   If the first `cell run` requires an account, the funnel dies at the
   door. Free local must genuinely be the best Docker killer on the Mac,
   period, with no visible agenda.
2. **OrbStack already lives in that market** (beloved Docker Desktop
   replacement). Cell's difference cannot be "a lighter VM" — it must be
   the CAS model: instant pull, dedup, memoization. Different category, but
   devs will compare on day one.
3. **Sequencing matters.** Finish Runners M1 → extract `Engine` + clw into
   the local binary → launch the free tier. Launching local before compute
   is ready creates demand that cannot convert.
