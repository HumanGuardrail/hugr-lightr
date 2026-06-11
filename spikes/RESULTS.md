# Spike results

## S4 — clonefile storm (2026-06-11, overnight)

**Machine:** MacBook Pro Intel i7-9750H (x86_64, APFS), under load from
concurrent sessions. **Method:** `s4-clonefile-storm.rs`, serial loop,
1 KiB files + one 64 MiB file; runs at n=10 000 (TMPDIR) and n=2 000
(repo volume) — consistent results.

| Op | per-file (1 KiB) | 64 MiB file |
|---|---|---|
| create | ~1.3–1.8 ms | — |
| clonefile | ~2.0 ms | ~instant (within noise) |
| fs::copy | ~2.2 ms | dominates copy phase |

**Findings (calibrate ADR-0009/0013 + budgets):**
1. On this machine class, per-file metadata syscalls (~2 ms) dominate —
   clonefile ≈ copy for small files (×1.1–1.2). The CoW latency win is on
   **large files**; the small-file win is **disk (zero duplication)**, not
   speed.
2. Therefore O(files) materialization cannot hit ≤150 ms @10k on this
   hardware no matter the rung — **empirical justification for ADR-0013
   (O(1) views)**: the only way to make materialization constant is to not
   materialize.
3. Parallelism (rayon, ~6 cores) should yield ×4–6 on the storm → ~3–5 s
   @10k serial→parallel on this box. Apple Silicon class is expected
   ~10–40× faster on these syscalls (to be measured on such hardware —
   tense law: not claimed until measured).
4. Budgets recalibrated (build-spec v2 §9 note): B3/B5-class numbers are
   **machine-class-relative**; CI budgets bind to this Intel box
   (B3′ hydrate 10k warm ≤5 s parallel CoW; B5′ snapshot 10k warm
   ≤2.5 s cold-hash, ≤500 ms warm-index); the whitepaper's ~ms targets
   bind to views (R2) and Apple Silicon, and stay unclaimed until the
   bench signs them on that hardware.

**Status:** S1 (NFS-loopback), S2 (VZ save/restore), S3 (composefs),
S5 (Apple kernel + Rust PID1) — pending, need owner session (downloads/
VMs/platform choices). S4 ✅ done.
