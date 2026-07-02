# ADR-0010 — The stat-index

- **Status:** Accepted (2026-06-11) (ratified 2026-07-02 under explicit owner delegation to TechLead; basis: implemented + gate-green per docs/spec/parity-audit.md)
- **Date:** 2026-06-11

One line: a git-style per-workspace index — `path → (size, mtime_ns,
inode, mode) → digest` — stored store-side (`~/.lightr/index/<root-hash>`,
never polluting the user's tree), making `status`/`snapshot`/`run`-keys
stat-walk-fast: only changed files are ever rehashed.

## Context
The bar kills full re-walk+rehash per operation (clw model). Git's index
has 20 years of production proof, including the "racily clean" hazard and
its fix.

## Decision
1. Binary, mmap-able, path-sorted index; atomic rewrite (temp+rename).
2. Parallel walk (ignore-aware: `.gitignore` + `.lightrignore`, skip
   `.git/`); stat match ⇒ trust digest; mismatch ⇒ rehash (BLAKE3,
   rayon).
3. Racily-clean handling: entries whose mtime equals index write-time are
   re-verified (the git rule).
4. The index feeds three verbs (status/snapshot/run-keys) — one mechanism;
   budgets B5/B6 and the B2 memo-key path hang off it.
5. Index is a cache, not truth: deletable at any time; cold rebuild is the
   slow-path walk.

## Consequences
Warm `snapshot` ≤100 ms and `status` ≤50 ms @10k files become possible;
`run` memo keys stop scaling with input size. Supersedes the
clw-`build_manifest_local`-per-call model.
