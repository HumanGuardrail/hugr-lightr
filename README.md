# HuGR Lightr

> **So light it isn't there.**

HuGR Lightr is a daemonless, imageless runtime: a single static binary that
materializes workspaces from CoreLink's content-addressed store in seconds,
runs them with near-zero overhead — native on macOS, microVMs in the cloud —
and skips execution entirely when the result is already cached.

```
$ brew install hugr-lightr         # bin: lightr
$ lightr run @hugr/web -- pnpm dev
⚡ hydrated 1.2 GB in 3.1s (94% local cache)
▶ running native — 0 MB overhead
```

## The bet

Docker is three products glued together, and the glue is why it is heavy:

1. **Distribution** — images, layers, registries
2. **Isolation** — namespaces, cgroups (a VM on macOS)
3. **Lifecycle** — a daemon running 24/7

Lightr unbundles them. Distribution is replaced by CoreLink's CAS (chunk-level
dedup beats layer tarballs), the daemon is deleted (one static binary, no
background process), and isolation becomes à la carte — none for trusted
local dev, namespaces on trusted Linux, Firecracker microVMs for hostile
multi-tenant cloud.

The isolation primitives are commodity (~5% of the value). The
content-addressed substrate underneath — instant pulls, chunk-level dedup,
memoized execution — is CoreLink, and it is already in production (~95% of
the value). Dedup is intra-tenant at GA; cross-tenant is designed-in and
staged (`CAP-DEDUP-CROSS-TENANT`).

## Status

**R0 delivered (2026-06-11, overnight wave): the warp core works.**
A 1.9 MB release binary; 90 tests green incl. the A1–A8 acceptance suite
end-to-end; `lightr bench --check` green on the Intel dev box (snapshot
warm 233 ms, status 34 ms, memo HIT 51 ms k files — see
`spikes/RESULTS.md`; ~ms targets bind to R2 views + Apple Silicon, tense
law). Whitepaper v2 (working backwards) is canon. The platform it
converges with already exists across three sibling repos:

| Layer | Repo | Status |
|---|---|---|
| CAS/AC storage | `corelink-server` | live in production |
| Workspace snapshot/hydrate/memoize | `corelink-workspaces` (`clw`) | shipped |
| Leased, isolated execution (`Engine` trait) | `corelink-runners` | core shipped, M1 fabric pending |

Lightr promotes the runners' internal `Engine` seam into a public, local-first
product. Sequencing note: launch after Runners M1, so the demand the free
tier creates has somewhere to convert.

## Quickstart (today, on this machine)

```
$ cargo build --release          # bin: target/release/lightr (1.9 MB)
$ lightr snapshot --dir . --name @me/proj
$ lightr hydrate /tmp/fresh --name @me/proj     # CoW, instant-ish
$ lightr run --input src -- make test           # memoized: 2nd run = HIT
$ lightr status --name @me/proj --json          # agent-ready output
$ lightr bench --vs-docker                      # run the table yourself
```

Nothing runs between invocations (`pgrep lightr` proves it). No daemon,
no images, no network code in the binary.

## Docs

- [`docs/VISION.md`](docs/VISION.md) — the problem, the funnel, the economics
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — execution model, isolation tiers, CoreLink seams
- [`docs/MVP-v0.1.md`](docs/MVP-v0.1.md) — first slice scope and open questions
