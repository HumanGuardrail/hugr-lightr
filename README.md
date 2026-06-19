# HuGR Lightr

> **So light it isn't there.**

HuGR Lightr is a daemonless, imageless runtime: a single static binary that
materializes workspaces from a content-addressed store, runs them with
near-zero overhead, and skips execution entirely when the result is already
cached.

```
# What runs today (build from source — see Quickstart):
$ lightr snapshot --dir . --name @me/proj
$ lightr hydrate /tmp/fresh --name @me/proj        # content-addressed, CoW
$ lightr run --input src -- make test              # memoized: 2nd run = HIT
$ lightr oci import alpine.tar --name @docker/alpine  # OCI/docker tar → store
$ lightr run --rootfs @docker/alpine -- echo hi    # CoW rootfs, memoized
$ lightr build -t @app/web .                       # Dockerfile, step-memoized
$ lightr bench --vs-docker                          # run the table yourself
```

> **Honest status (read this first).** What ships today is the **local**
> engine — store, memoized `run`/`build`, OCI import (sha256-verified), the
> time-axis verbs, lazy compose, docker compat, and the agent/MCP surface —
> genuinely fast and fully tested (411 tests, 0 failures). Workspace
> materialization ships as **CoW hydrate** (real + tested). The **`vz` engine
> is runtime-validated end-to-end on Intel x86_64** — `lightr run --engine vz`
> boots a real microVM and returns the guest's real exit code (F-205/F-206);
> the arm64 sibling is **press-go on Apple Silicon** via the runbook in
> `spikes/s5-vz-boot-arm64/` (code-complete, not yet claimed validated).
> What is **NOT yet validated/built**: the `ns` (Linux) and `wsl` (Windows)
> engines are code-complete but hardware-gated (runbooks/CI, none claimed
> validated); the **O(1) "views" backends** (composefs/NFS-loopback/projfs)
> are a planned perf optimization (ADR-0013 spike, honest `Unsupported`,
> unwired — the shipped runtime already materializes via CoW hydrate); and a
> published `brew`/release is owner-gated (`G-PUBLISH` — the project is
> **Apache-2.0** per ADR-0008, naming cleared, metadata ready; see
> [`docs/RELEASE.md`](docs/RELEASE.md)). The headline ~ms / boot-never perf
> targets bind to the O(1) views layer + Apple Silicon and remain **targets,
> not measurements** (the measured release numbers are in `spikes/RESULTS.md`).
> Full feature-by-feature truth ledger:
> [`docs/spec/parity-audit.md`](docs/spec/parity-audit.md).

## The bet

Docker is three products glued together, and the glue is why it is heavy:

1. **Distribution** — images, layers, registries
2. **Isolation** — namespaces, cgroups (a VM on macOS)
3. **Lifecycle** — a daemon running 24/7

Lightr unbundles them. Distribution is replaced by CoreLink's CAS (chunk-level
dedup beats layer tarballs), the daemon is deleted (one static binary, no
background process), and isolation becomes à la carte — none for trusted
local dev, namespaces on trusted Linux, a Virtualization.framework microVM
(`vz`, validated on Intel macOS) for Linux-on-Mac, and Firecracker (`fc`,
staged) for hostile multi-tenant cloud.

The isolation primitives are commodity (~5% of the value). The
content-addressed substrate underneath — instant pulls, chunk-level dedup,
memoized execution — is CoreLink, and it is already in production (~95% of
the value). Dedup is intra-tenant at GA; cross-tenant is designed-in and
staged (`CAP-DEDUP-CROSS-TENANT`).

## Status

**R0–R4 + go-live hardening delivered (2026-06-17).** A **~4.5 MB** stripped
release binary (measured, `bench B7`; ≤10 MB target met); **411 tests, 0 failures**, clippy
`-D` clean (default + `--features vz`), fmt clean; `lightr bench --check` green
on the Intel dev box (snapshot warm 233 ms, status 34 ms, memo HIT 51 ms — see
`spikes/RESULTS.md`; the ~ms / boot-never targets bind to the O(1) views layer +
Apple Silicon, tense law). The `vz` engine is runtime-validated end-to-end on
Intel x86_64 (F-205/F-206). Whitepaper v2 (working backwards) is canon. The
platform it converges with already exists across three sibling repos:

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
$ cargo build --release          # bin: target/release/lightr (~4.5 MB, stripped)
$ lightr snapshot --dir . --name @me/proj
$ lightr hydrate /tmp/fresh --name @me/proj     # CoW, instant-ish
$ lightr run --input src -- make test           # memoized: 2nd run = HIT
$ lightr status --name @me/proj --json          # agent-ready output
$ lightr run --rootfs @docker/alpine -- echo hi # CoW rootfs (native engine)
$ lightr bench --vs-docker                      # run the table yourself
```

Boot a real Linux microVM on Intel macOS (validated end-to-end, F-205/F-206 —
needs `--features vz`; the arm64 sibling is press-go via `spikes/s5-vz-boot-arm64/`):

```
$ cargo build --release --features vz
$ lightr run --engine vz --rootfs @docker/alpine -- /bin/sh -c 'exit 7'  # → 7
```

Command surface beyond the verbs above: `lightr --version` (git-sha +
build-date), `lightr completions <shell>`, `lightr man`, `lightr schema`,
`lightr mcp` (the agent/MCP server), plus `ps`/`logs`/`exec`/`stop`,
`undo`/`diff`/`bisect`, `build`/`compose`/`docker`, and `gc`.

Nothing runs between invocations (`pgrep lightr` proves it). No daemon,
no images. The local verbs touch no network; only `oci pull` reaches a
registry (the quarantined bridge — ADR-0011).

## Docs

- [`docs/VISION.md`](docs/VISION.md) — the problem, the funnel, the economics
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — execution model, isolation tiers, CoreLink seams
- [`docs/MVP-v0.1.md`](docs/MVP-v0.1.md) — first slice scope and open questions
