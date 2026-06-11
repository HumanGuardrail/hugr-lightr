# HuGR Lightr — Product Whitepaper v2 (working backwards)

> **This is a working-backwards artifact: it describes the FINISHED product,
> written before the product exists.** Nothing below is shipped; every number
> is a target until `lightr bench` measures it (tense law). The build tracks
> this document; this document does not track the build. Supersedes v1 as
> canon. 2026-06-11.

## Abstract

Lightr is what container tooling looks like when nothing is allowed to
weigh anything. One static binary, no daemon, no images, no layers: a single
content-addressed plane where workspaces, Linux environments, VM states,
build steps and logs are all the same kind of object — a **ref** — and
everything the user sees is an instant, lazy **view** over that plane.
Workspaces appear in constant time regardless of size. Linux runs without
anyone ever watching it boot. Identical work is never executed twice.
Twelve-service stacks idle at zero. And every operation can explain itself,
in machine-readable form, because Lightr is built for a world where the
operator is as likely to be an LLM agent as a human. It delivers everything
Docker, OrbStack and Apple's `container` do — materially faster, lighter
and more transparent than all three — because they optimize the container,
and Lightr deletes the container's costs.

## 1. The scene

> **Aspirational scene** (working-backwards — this is the target, not a
> recording). The Linux/microVM parts below are NOT yet runnable; see the
> README "Honest status" box and `docs/spec/parity-audit.md` for what
> executes today vs. what binds to Apple Silicon + the views layer.

A laptop that has never seen the project:

```
$ brew install hugr-lightr                  # ≤10 MB. While you read this
                                            # line, the golden VM booted
                                            # once, suspended, and will
                                            # never boot again.
$ lightr run @hugr/monorepo -- make test    # 100k files, Linux toolchain
⚡ view mounted (workspace appears, O(1))    # ~ms — solidifying behind you
⚡ linux resumed (nobody booted anything)    # ~100–300 ms
⚡ memo: 3 188/3 200 steps already known     # your team built this today
✓ tests green — 1.4 s wall clock, first run, cold machine
$ ps aux | grep lightr                      # nothing. it isn't there.
```

The Docker user is still watching a progress bar. Then you run
`lightr bench --vs-docker` and publish the table yourself.

## 2. One noun

Docker ships six mental models (image, container, volume, build cache,
registry, compose project) with six lifecycles and six ways to leak disk.
Lightr ships one: the **ref** — a content-addressed, immutable,
lineage-tracking name for any tree of bytes. A workspace is a ref. An OS
userland is a ref. A suspended VM is a ref. A build step's result is a ref.
`lightr gc` is the only janitor; lineage makes time travel free; content
addressing makes dedup, integrity and distribution one mechanism instead of
six. Nix proved this physics and fumbled the product; Docker proved the
product and fumbled the physics. Lightr is both, on a cache platform that
already bills customers.

## 3. The content plane (the store)

- **File-level CAS**, BLAKE3-keyed, immutable, sharded; objects are
  protected by the kernel where the kernel can (fs-verity on Linux),
  read-only mode everywhere.
- **Big-object exception:** VM memory states and other giant blobs are
  **page-aligned chunked** (the CodeSandbox trick) so suspended machines
  dedup across projects and sync incrementally.
- **CoW ladder**, probed once per store: `clonefile` (APFS) → `FICLONE`
  (btrfs/XFS) → `copy_file_range` → copy. The bench reports your rung; the
  microwave clause guarantees correctness on all rungs.
- **Binary, mmap-able manifests** (sorted, git-style). JSON never sits on a
  hot path; parsing is not an operation Lightr performs while you wait.
- Wire format to CoreLink remains FastCDC chunks + BLAKE3 — computed in
  background at push, never on the hot path (the clw seam, quarantined).

## 4. Views: materialization in O(1)

`hydrate` does not copy a workspace into existence — it **mounts a view**
of the manifest: the workspace appears in constant milliseconds whether it
holds 1k or 1M files (composefs/EROFS on Linux — kernel-native, no FUSE;
NFS-loopback on macOS — the EdenFS-proven route; FSKit when it matures).
Behind the view, a **solidifier** races your usage, promoting hot files to
native CoW clones on real disk; when it finishes, the mount evaporates and
you are on bare APFS/ext4 with zero indirection. First access: instant.
Steady state: native. Corollary: **`run` starts before data arrives** —
execution begins in milliseconds against the view; whatever the process
touches faults in, by priority, while it runs. "Pull then run" is dead.

## 5. Execution: engines that aren't there

One `Engine` contract (spawn/probe/exec/teardown — the runners lineage),
five implementations, chosen per context, none resident:

- **`native`** — posix_spawn, <5 ms, zero isolation *and says so loudly*:
  reproducibility, not a sandbox. Supervision without a daemon: the run's
  own process tree owns its control socket; `exec/logs/stop` are
  peer-to-peer; everything dies together.
- **`ns`** (Linux) — clone3 + pivot_root **directly into the CoW-cloned
  tree**. Overlayfs is deleted from the design: the store already provides
  instant writable trees. ~10–20 ms, crun-class, no storage driver.
- **`vz`** (macOS) — microVMs that **nobody ever boots**: the golden state
  is minted once per machine, in the background, at install; every Linux
  run thereafter is a ~100–300 ms resume. Per-project warm states suspend
  with your toolchain loaded and page cache hot. Kernel: derived from
  Apple's open-source Containerization kernel (anti-NIH) + a ~1 MB static
  Rust PID1. Guest sees the **store** (immutable) via virtiofs and builds
  its view inside with composefs — **immutability turns the host↔guest
  boundary from a chatty protocol into a content cache** (the structural
  answer to OrbStack's bridge). Rosetta runs x86 images near-native;
  virtio-balloon keeps guests at 128–256 MB baseline. Idle-zero is kept by
  TTL suicide, not by never existing.
- **`fc`** (cloud) — Firecracker snapshot-resume (~5 ms) on the Runners
  fabric. Local and cloud are one store, one law.
- **`docker`** (compat) — the migration ramp, not the model.

## 6. The verbs

Full surface, every verb idle-free:

`run` (memoized; child's exit code; HIT/MISS on stderr) · `snapshot`
(≤100 ms warm via stat-index) · `hydrate` (O(1) view) · `status` (index
diff) · `exec/logs/ps/stop` (peer-to-peer, daemonless) · `build`
(Dockerfile-compat; every step a content-keyed run — see §8) ·
`compose` (§7) · `oci import/export` + registry push/pull (layers unpacked
once at the border, CoW forever after) · `vm` (suspend/resume/list states
as refs) · `undo` / `diff @ref@{yesterday}` / `bisect` (§ time axis) ·
`bench --vs-docker` (§10) · `gc` (one janitor) · `mcp` (§9).

**The time axis** — append-only store + lineage + memoized runs make Lightr
a time machine for free: `undo` any snapshot, `diff` across history,
`bisect` over workspace states running the memoized test — O(log n) steps,
mostly cache hits, often fully offline. Docker does not have a bad version
of this; it has no version of this.

## 7. Compose without residency

`lightr compose up` on a 12-service stack starts **zero services**: it
registers 12 listeners of a few KB (socket activation as the default, not
a systemd curiosity). The first SELECT resumes postgres from its suspended
state; the first GET wakes redis. `up` returns in milliseconds; services
you didn't touch today never existed today; an idle "running" stack costs
~0 RAM. An 8 GB laptop runs a 16-service stack. Per-stack ephemeral
supervisor with TTL — consistent with the no-resident-daemon law.

## 8. Build: memo all the way down

`lightr build` is Dockerfile-compatible on the outside and a different
animal inside: every step is a run whose key is the content of everything
it actually read. The dependency oracle is the filesystem view itself — we
own the FS, so we see every open (the tup×BuildXL synthesis, robust on
Linux, spawn-shim nitro on macOS). Unchanged steps are lookups; changed
steps rebuild alone; with Stage 2, your teammate's compiler already
compiled your file. BuildKit's fragile local cache against a
content-addressed action cache shared by your team: not a fair fight.

## 9. Born for the LLM era

The operator of a container runtime in 2026 is as likely to be an agent as
a human — and HumanGuardrail builds for exactly that world. Lightr is the
first runtime designed agent-first:

- **`--json` on every verb** — stable, versioned schemas; terse by design
  (output is context-window-budgeted; no banners for machines).
- **`--explain`** — every operation narrates itself structurally: why this
  was a cache hit (key composition, input digests), what materialized, which
  CoW rung you got, what a `run` actually read. Transparency as a feature,
  not a debug flag.
- **`plan` mode** — any mutating verb dry-runs: what would materialize,
  execute, or change — agents preview before they act.
- **`--events ndjson`** — a structured event stream for orchestrators.
- **`lightr mcp`** — the runtime IS an MCP server: agents get
  run/snapshot/hydrate/diff as native tools, with the same zero-idle law.
- **Agent sandboxes as a first-class workload** — ephemeral microVMs,
  content-addressed inputs, attested outputs, memoized repeats: the safest
  and cheapest place for an agent fleet to execute code, locally today and
  on Runners at scale tomorrow.
- **Determinism as trust** — content addressing means an agent (or its
  human) can verify byte-for-byte what ran, from what inputs. In an era of
  guardrails, the runtime itself is one.

## 10. The records (measured, or not claimed)

`lightr bench --vs-docker` ships in the binary: it runs the table below on
*your* machine and prints both columns. Until it runs, these are targets:

| Indicator | Docker Desktop | OrbStack | Apple container | Lightr target |
|---|---|---|---|---|
| Idle RAM | 2–4 GB | low, ≠0 | ~0 | **0** |
| CLI overhead | 50–150 ms | similar | ~ms | **<5 ms** |
| Materialize 1 GB / 100k files | 30–60 s | layers, fast | layers | **O(1) view, ~ms** |
| Linux start | shared VM, warm | ~1–2 s VM | sub-s boot/ctr | **~100–300 ms resume; boot never seen** |
| Native run (no Linux needed) | n/a — everything pays the VM | n/a | n/a | **<5 ms, no VM at all** |
| Re-run identical | full | full | full | **≤10 ms replay** |
| Snapshot 10k files warm | seconds–min | seconds | n/a | **≤100 ms** |
| 12-service stack idle | 12 alive | 12 alive | 12 VMs | **~0 (listeners only)** |
| Build, nothing changed | cache-dependent | same | same | **O(ms), lookups only** |
| Install | ~1.5 GB | ~hundreds MB | OS-bundled | **≤10 MB** |

CI carries these as budgets; a regression is a red gate with the same
status as a failing test.

**Measured so far** (release 1.9 MB binary on an Intel i7-9750H dev box
under load — `lightr bench`; Apple-Silicon + views numbers stay unclaimed
until measured there, tense law):

| Indicator | Lightr target | Measured (this box) |
|---|---|---|
| Idle RAM (between runs) | 0 | **0** — `pgrep` empty (A4) |
| CLI overhead (`--version`) | <5 ms | **~7 ms** (debug ~7; within machine-class) |
| Native run, memo HIT | ≤10 ms | **~51–77 ms** end-to-end (re-validates inputs via stat-walk; ~ms target binds to R2 views) |
| Snapshot 10k warm | ≤100 ms | **~233 ms @2k** (stat-index; Intel HDD-syscall bound) |
| Install (binary) | ≤10 MB | **1.9 MB** |
| Materialize (CoW) | O(1) view | **CoW clone, rung=Clone** (O(files) on Intel; O(1) views = R2) |
| OCI import / build-cached / compose-up | measured | **bench B9/B10/B11 green** |

The honest gap: this is an **Intel** box where per-file metadata syscalls
(~2 ms) dominate, so the sub-10 ms / O(1) headline numbers bind to the
views layer (R2) and Apple Silicon — they are *targets with a mechanism*,
not yet *measurements*. Everything the local product actually does is
green and tested (338 cases, A1–A30); see `docs/spec/parity-audit.md`.

## 11. What we absorbed (and from whom)

- **From Docker:** the verbs, the Dockerfile/compose/OCI compatibility, the
  ecosystem ramp. Parity is table stakes; the model underneath is replaced.
- **From OrbStack:** the UX bar — per-container DNS, VPN coexistence,
  Mac-native polish. Its ceiling is structural: it ships dockerd in a VM,
  so it can never leave the registry/layer/mutable-bind-mount model.
- **From Apple `container`:** the kernel (open source, tuned — adopted, not
  reinvented) and the public validation of microVM-per-job. Its ceiling:
  OCI physics, boot-fast-not-never, no store, no continuity.
- **From Nix/EdenFS/GVFS/composefs/BuildXL/CodeSandbox:** each proved one
  organ of this architecture somewhere (internal, abandoned, cloud-only or
  UX-cursed). Lightr is the first assembly of all of them as one consumer
  product — anchored on a production CAS none of them have.

## 12. Platform continuity (why this wins as a business)

Local Lightr is free forever, no account, COGS ≈ 0. The store's refs are
the on-ramp: wanting a ref on a second machine or a teammate's laptop is a
flag-flip into a CoreLink tenant (Stage 2); once the team's refs live in
the CAS, CI and agent fleets that run anywhere else are paying to download
what already sits in your cache — Runners and Workspaces sell themselves
(Stage 3). One store, one law, laptop → team → CI → cloud. Dedup tense:
intra-tenant at GA; cross-tenant staged (`CAP-DEDUP-CROSS-TENANT`) — the
network-effect moat depends on it landing and is stated as such.

## 13. Principles (decided — owner-only to relitigate)

1. **No resident daemon, ever.** Ephemeral, scoped, TTL-suicidal helpers
   only. The OS's supervisor handles restart policies.
2. **No images, no layers** past the OCI border. Refs and views.
3. **Every feature exists; no feature weighs until invoked.** Idle = 0.
4. **Content never moves; views change.** O(1) appearance, background
   solidification, run-before-data.
5. **Memoize first.** The cheapest run is the one that never happens.
6. **Boot is a build-farm/install-time event**, never a user experience.
7. **Free local, forever, no account.** The funnel dies at the first login
   wall.
8. **Pure client of CoreLink.** Zero server changes; CoreLink's law
   (including tense) governs the wire.
9. **Fail closed.** Integrity errors are loud; partial results don't exist;
   `native` says "not a sandbox" out loud.
10. **Agent-first interfaces** (§9) are core surface, not an SDK
    afterthought.
11. **Claim only what the bench measured.** The records table is a
    commitment map until the harness signs it.
12. **Ship after Runners M1.** Demand must have somewhere to convert.

## 14. What we refuse

A second resident daemon under any feature pressure · databases on hot
paths · overlayfs (CoW replaced it) · async runtimes in the core (rayon +
syscalls; tokio quarantined at the wire) · JSON on hot paths · cluster
orchestration (Runners' fabric, not ours) · Linux-syscall-emulation on
macOS (the WSL1 swamp — VMs won that argument) · prose where a schema
serves an agent better.

## 15. The road

- **Spikes S1–S5** (de-risk, before any production code): NFS-loopback
  build overhead · VZ save/restore latency + config/Rosetta constraints ·
  composefs over our store + fs-verity · clonefile storm at 100k ·
  Apple-kernel + Rust PID1 boot time. Spike numbers calibrate the budgets.
- **R0 — the warp core:** store + index + views + native engine + memoized
  run + bench harness. The brew-install moment lives here.
- **R1 — runtime parity:** volumes/exec/logs/limits, rootless, control
  sockets.
- **R2 — the Linux tier:** vz boot-never, OCI border, networking (DNS/VPN
  at OrbStack's bar), Rosetta.
- **R3 — ecosystem:** build deep-memo, lazy compose, docker-CLI/socket
  compat, launchd/systemd integration.
- **R4 — beyond:** time axis verbs, `lightr mcp`, agent sandbox profiles,
  LAN mesh, Stage-2 sync.

Gates: ADRs hammered by the owner before code · spike numbers before
budgets · acceptance + bench green before any ring is claimed · license
(ADR-0008) before any public artifact.

## 16. Why HuGR wins

Because the hard two-thirds already run in production under this roof —
the CAS with paying tenants, the Action Cache, the workspace pipeline, the
fail-closed execution core — and the remaining third is commodity physics
assembled with discipline. Competitors must build our moat to copy our
product; we only have to build our product on our moat. And the name is
the KPI: if it isn't insanely light, it isn't Lightr.
