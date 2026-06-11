# ADR-0009 — The content plane: one store for everything

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to
  morning review; see `../decisions-log.md`)
- **Date:** 2026-06-11

One line: a single content-addressed plane — **file-level** CAS objects
(BLAKE3, immutable, sharded) for workspaces, **page-aligned chunked**
objects for giant blobs (VM states), binary mmap manifests, refs with
lineage — replacing ADR-0003's clw-pipeline store as the local model;
FastCDC chunks survive only as the wire format (ADR-0011).

## Context
The performance bar killed byte-copy hydration; CoW (`clonefile`) requires
whole-file objects; VM states dedup poorly file-level (CodeSandbox proves
page-aligned chunking); JSON manifest parse (~100 ms @100k entries) cannot
sit on a hot path. EdenFS/GVFS/composefs prove store+view at scale.

## Decision
1. Objects: `objects/<2hex>/<62hex>`, write-once (temp+rename), immutable,
   chmod read-only; **fs-verity sealing on Linux (R2)**.
2. Big-object class: page-aligned chunk recipes for blobs > threshold
   (default 64 MiB) — the only chunked thing in the local store.
3. Manifests: custom binary, path-sorted, mmap-able; digest over canonical
   bytes. JSON only at borders (`--json`, wire).
4. Refs: `name → (manifest digest, parent, created_at, tool_version)` —
   the lineage that powers `undo`/`diff`/`bisect` (F-401/402).
5. Integrity fail-closed: get() rehashes (until fs-verity does it);
   mismatch = `Integrity` error, evidence preserved, never silent-deleted.
6. CoW ladder probed at store init: clonefile → FICLONE →
   copy_file_range → copy; rung recorded, surfaced in `--explain`/bench.
7. Default root `~/.lightr/store` (`LIGHTR_STORE_DIR` override). The L1
   cache concept from v0.1 is **deleted** — the store IS local.

## Consequences
- Hydration = CoW clone of objects (R0) and O(1) views (ADR-0013, R2);
  zero extra disk until mutation.
- Local dedup is file-level (loses sub-file dedup; wins clonability) —
  accepted trade, wire keeps chunk-level dedup.
- Supersedes ADR-0003 (clw-store) — 0003 marked Superseded.
