# ADR-0003 — LocalStore: local CAS/AC, fail-closed

- **Status:** Superseded by ADR-0009 (content plane)
- **Date:** 2026-06-11

One line: `lightr-store::LocalStore` implements clw's `CasTransport` +
`AcTransport` over a plain directory tree, with atomic writes, fail-closed
integrity on read, the trait's 5 MiB blob cap enforced, and last-write-wins
AC semantics — making Stage-1 "touches no servers" structurally true.

## Context

clw pipelines accept any `C: CasTransport + AcTransport`. The HTTP client is
the only existing impl; Stage 1 requires a local one. clw's `LocalCache` is
an L1 *cache* (self-healing by deletion); a *store* is the local source of
truth and must not silently destroy evidence.

## Decision

1. **Layout** (sharded like clw-cache, two top-level namespaces):
   ```
   <root>/cas/<2-hex>/<64-hex>     # content blobs, keyed by BLAKE3 digest
   <root>/ac/<2-hex>/<64-hex>      # AC values, keyed by Digest hex
   ```
   Default root `~/.lightr/store`; override via `LIGHTR_STORE_DIR`.
2. **Atomicity:** every write is temp-file + rename within the same shard
   directory (same filesystem, POSIX-atomic).
3. **Integrity, fail-closed:** `CasTransport::get` re-hashes the blob; a
   mismatch returns `ClwError::Integrity { expected, actual }` and **does
   not delete** the corrupt file (a store is evidence; deletion is the
   cache's semantics, not the store's). Missing blob → `ClwError::NotFound`.
4. **Cap:** `put` enforces `CAS_BLOB_CAP_BYTES` (5 MiB) → `ClwError::TooLarge`,
   honoring the trait's documented contract.
5. **AC semantics:** `get` → `Ok(None)` when absent; `put` overwrites
   (last-write-wins), mirroring the server.
6. **Concurrency posture:** single-user CLI; filesystem-level atomicity is
   the guarantee; no locking daemon (there is no daemon).
7. **Accepted cost (documented, not debt):** clw pipelines also maintain the
   L1 `LocalCache` (`~/.lightr/cache`, override `LIGHTR_CACHE_DIR`), so touched
   blobs exist twice on disk (~2× working set). Price of consuming clw
   pipelines unmodified; disappears with the lazy-rootfs work (v1.x).

## Consequences

- Offline-complete by construction; the acceptance suite asserts it.
- Corruption is detected and reported, never papered over — consistent with
  the platform's fail-closed law.
