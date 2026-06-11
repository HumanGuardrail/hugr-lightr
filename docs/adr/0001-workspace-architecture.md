# ADR-0001 — Workspace & crate architecture

- **Status:** Proposed
- **Date:** 2026-06-11

One line: a three-crate Cargo workspace — `cell-store` (local CAS/AC),
`cell-cli` (the `cell` binary), `cell-acceptance` (end-to-end suite) — with a
strict one-way dependency rule: `cell-cli → cell-store`, nothing depends on
`cell-cli`, `cell-acceptance` depends only on the built binary.

## Context

v0.1 (`docs/MVP-v0.1.md`) is `cell run/snapshot/hydrate/status`, local-only,
native engine, built on clw pipelines. The clw pipelines are generic over
`CasTransport + AcTransport` (verified 2026-06-11 against
`corelink-workspaces` @ `f8f5edf`), so the only genuinely new substance is a
local filesystem transport + the CLI surface.

## Decision

```
hugr-cell/
  Cargo.toml                 # workspace (lead-owned; agents never touch)
  crates/
    cell-store/              # LocalStore: CasTransport + AcTransport over disk (ADR-0003)
    cell-cli/                # bin `cell`: snapshot/hydrate/status/run over clw pipelines
    cell-acceptance/         # assert_cmd end-to-end suite (A1–A7 in the build spec)
```

- Dependency rule: `cell-cli → cell-store`; `cell-store` depends only on
  clw crates + std needs; `cell-acceptance` invokes the compiled `cell`
  binary (no library dependency on either crate).
- Binary name `cell`; crate names are workspace-internal (`cell-store`,
  `cell-cli`). Any future crates.io publication uses the `hugr-cell` package
  name (crates.io `cell` is taken) — not exercised in v0.1 (`publish = false`).
- No `Engine` trait crate in v0.1 — see ADR-0005.

## Consequences

- Three disjoint write-scopes → the v0.1 wave parallelizes conflict-free.
- The acceptance crate exercising only the binary keeps it honest as a
  product test (no internal shortcuts) and decouples it from refactors.
