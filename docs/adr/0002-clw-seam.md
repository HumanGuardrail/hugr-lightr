# ADR-0002 — clw seam: path-dependencies on the sibling repo

- **Status:** Accepted — narrowed by ADR-0011 (clw direct path-dep deferred to Stage-2) (ratified 2026-07-02 under explicit owner delegation to TechLead; basis: implemented + gate-green per docs/spec/parity-audit.md)
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

## Update 2026-06-17 — reconciliation

The Decision text above ("v0.1 consumes clw crates via Cargo
path-dependencies on the sibling checkout") is **not v0.1 reality** and is
**superseded-in-part by ADR-0011**. Reconciling the record for go-live:

- **Narrowed by ADR-0011.** The perf rework removed clw from the hot path
  and pushed everything networked into quarantined bridge crates. The clw
  path-dependency therefore does **not** apply to the v0.1 core; ADR-0002's
  scope is narrowed to the bridge crates only (ADR-0011 §2).
- **clw direct path-dependency is DEFERRED to Stage-2.** The bridge crates
  that carry it (`lightr-wire`, R4; `lightr-oci`, R2) are Stage-2 surfaces.
  No v0.1 core crate takes a clw path-dep.
- **The v0.1 seam is the wire-bridge** at the CoreLink + OCI border —
  local↔wire conversion (file objects ↔ FastCDC chunk manifests / OCI
  layers) crossed in background, never on a hot path.
- **The code intentionally has no clw path-deps.** The absence of
  `clw-types`/`clw-cache`/… path-dependencies in the v0.1 workspace is
  correct and deliberate, not an omission — it reflects this reconciliation.

The original Decision text above this section is preserved unchanged as the
historical record; this Update governs.
