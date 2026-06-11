# HuGR Cell

> **The smallest unit of compute that lives on its own.**

HuGR Cell is a daemonless, imageless runtime: a single static binary that
materializes workspaces from CoreLink's content-addressed store in seconds,
runs them with near-zero overhead — native on macOS, microVMs in the cloud —
and skips execution entirely when the result is already cached.

```
$ brew install hugr-cell         # bin: cell
$ cell run @hugr/web -- pnpm dev
⚡ hydrated 1.2 GB in 3.1s (94% local cache)
▶ running native — 0 MB overhead
```

## The bet

Docker is three products glued together, and the glue is why it is heavy:

1. **Distribution** — images, layers, registries
2. **Isolation** — namespaces, cgroups (a VM on macOS)
3. **Lifecycle** — a daemon running 24/7

Cell unbundles them. Distribution is replaced by CoreLink's CAS (chunk-level
dedup beats layer tarballs), the daemon is deleted (one static binary, no
background process), and isolation becomes à la carte — none for trusted
local dev, namespaces on trusted Linux, Firecracker microVMs for hostile
multi-tenant cloud.

The isolation primitives are commodity (~5% of the value). The
content-addressed substrate underneath — instant pulls, cross-tenant dedup,
memoized execution — is CoreLink, and it is already in production (~95% of
the value).

## Status

**Design phase.** No code yet. The vision and architecture are written down;
the execution core it builds on already exists across three sibling repos:

| Layer | Repo | Status |
|---|---|---|
| CAS/AC storage | `corelink-server` | live in production |
| Workspace snapshot/hydrate/memoize | `corelink-workspaces` (`clw`) | shipped |
| Leased, isolated execution (`Engine` trait) | `corelink-runners` | core shipped, M1 fabric pending |

Cell promotes the runners' internal `Engine` seam into a public, local-first
product. Sequencing note: launch after Runners M1, so the demand the free
tier creates has somewhere to convert.

## Docs

- [`docs/VISION.md`](docs/VISION.md) — the problem, the funnel, the economics
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — execution model, isolation tiers, CoreLink seams
- [`docs/MVP-v0.1.md`](docs/MVP-v0.1.md) — first slice scope and open questions
