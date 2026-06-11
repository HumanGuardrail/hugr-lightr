# ADR-0002 — clw seam: path-dependencies on the sibling repo

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to morning review); ADR-0002 scope narrowed by ADR-0011 (bridge crates only)
- **Date:** 2026-06-11

One line: v0.1 consumes clw crates via Cargo **path-dependencies** on the
sibling checkout (`../corelink-workspaces/crates/*`), read-only, with
`publish = false` across the workspace; the transcribed-contract /
conformance-vector pattern is explicitly NOT adopted at this stage.

## Context

The house has two seam patterns: (a) direct dependency, (b) wire-level
transcribed types + byte-identical conformance vectors (hugit ↔
corelink-runners). Pattern (b) exists for **frozen seams between
independently-evolving repos with no code dependency allowed**. Lightr and clw
are same-owner, same-language, same-machine; clw is a client *library* by
design; `docs/product/product.md` §9 leaned toward direct dependency.

Facts verified 2026-06-11: clw workspace @ `f8f5edf` (clean tree), crates
`clw-types/-cache/-snapshot/-hydrate/-run/-manifest` all `edition 2021`,
`version 0.1.0`, `license UNLICENSED`, not published to crates.io.

## Decision

1. `Cargo.toml` uses path-deps:
   `clw-types = { path = "../corelink-workspaces/crates/clw-types" }` (etc.).
2. The sibling repo is **never mutated** from this repo (standing fence). If
   Lightr needs a clw change, it is requested via the owner/clw session.
3. The clw baseline consumed is **recorded** in the build spec
   (`corelink-workspaces @ f8f5edf`); if the sibling moves and breaks the
   build, the breakage is loud (compile error), and the fix is a deliberate
   re-baseline commit here — never a silent drift.
4. `publish = false` on every lightr crate until the distribution decision
   (ADR-0008 + product.md §9) is made.

**Revisit trigger:** first external distribution (brew/public repo) — at that
point choose: publish clw crates, vendor, or git-dep with pinned rev.

## Consequences

- Zero duplication, zero drift surface in v0.1; compile-time integration.
- Build requires the sibling checkout at the expected relative path —
  acceptable for the single-machine dev phase, recorded as a known
  constraint in the build spec.
