# The bar — obliterate, don't improve

- **Status:** Owner directive, 2026-06-11. This document outranks comfort.
- **Owner's words (verbatim, pt-BR):** "essa porra precisa ser warp speed,
  não é eficiente e melhor que o docker, é humilhar e obliterar o docker em
  todos os indicadores […] precisa ser muito leve, muito rápido, precisa
  voar, precisa fritar a mente de quem usar de tão bom e eficiente que é."

The product promise is not "better than Docker". It is **obliteration on
every indicator** — and the name now carries it: Lightr. Anything that adds
weight, latency, or a daemon answers to this document.

## Indicators & draft targets

Targets are DRAFT until the bench harness measures them (tense law: nothing
below may be claimed publicly until measured on real hardware). Docker
figures are typical observed magnitudes for a warm Docker Desktop on a dev
Mac, to be pinned precisely by the harness.

| # | Indicator | Docker (typical) | Lightr target | Mechanism |
|---|---|---|---|---|
| 1 | Install footprint | ~1.5 GB app + VM image | single binary ≤ 10 MB | static bin, no VM |
| 2 | Idle RAM / CPU | 2–4 GB, constant CPU | **0** (nothing runs) | no daemon |
| 3 | Materialize 1 GB workspace, warm | 30–60 s pull+unpack | **≤ 100 ms** | APFS `clonefile`/reflink CoW — metadata ops, zero byte copy |
| 4 | Re-run identical job | full re-run | **≤ 10 ms** total overhead | memo replay fast path |
| 5 | Snapshot ("commit") warm, 10k files | seconds–minutes | **≤ 100 ms** | stat-index: only changed files rehash |
| 6 | Disk for 10 similar workspaces | ~10× layers | **~1×** | file-level dedup + CoW clones |
| 7 | Runtime exec overhead | dockerd+runc path | **0** (native) / µs-ms | direct spawn |
| 8 | Cold start to first instruction | seconds | ms (native) | no VM, no daemon handshake |

## What the bar kills in the current spec (rework list)

The v0.1 build-spec (`build-spec-v0.1.md`) reused clw pipelines verbatim.
That design is **correct but not obliterating** — it fails indicators 3, 5
and 6:

1. **Byte-copy hydration** — store → L1 cache → workspace copies bytes
   (~2–3× disk, copy-speed materialization). → Replace with **CoW
   materialization** (`clonefile`/reflink), which requires a **file-level
   local object store** (chunks can't be cloned into a file without
   copying).
2. **Full re-chunk on every snapshot/status** — clw walks and re-chunks the
   whole tree each time. → **stat-index** (git's trick: path, size,
   mtime, inode → digest), parallel stat-walk, rehash only what changed.
3. **Input re-hash on every `run`** — same fix as 2 (the index feeds the
   memo key).
4. **No measurement** — "every indicator" requires a **bench harness vs
   Docker as a first-class artifact**: budgets in CI (regression = red
   gate), published table as marketing.

Chunk-level FastCDC does not die: it moves to the **wire** (Stage-2 sync
with CoreLink), computed in background at push — the hot local path never
waits for it. The CoreLink seam stays a pure client; the wire format stays
CoreLink law.

## Consequences (status changes)

- ADR-0003 (LocalStore) and ADR-0005 (engine/run path) → **UNDER REWORK**.
- `build-spec-v0.1.md` §3/§4/§5/§7 → not freezable; superseded by the
  rework. No freeze, no wave, until the performance architecture ADRs
  (0009+: CoW store, stat-index, wire bridge, bench doctrine) are written
  and Accepted.
- ADRs 0001 (3-crate shape), 0002 (clw seam — now scoped to the wire
  bridge), 0004 (grammar), 0006 (toolchain), 0007 (telemetry) survive the
  bar unchanged.
