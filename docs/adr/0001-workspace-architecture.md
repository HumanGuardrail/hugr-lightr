# ADR-0001 — Workspace & crate architecture

- **Status:** Proposed
- **Date:** 2026-06-11

One line: a three-crate Cargo workspace — `lightr-store` (local CAS/AC),
`lightr-cli` (the `lightr` binary), `lightr-acceptance` (end-to-end suite) — with a
strict one-way dependency rule: `lightr-cli → lightr-store`, nothing depends on
`lightr-cli`, `lightr-acceptance` depends only on the built binary.

## Context

v0.1 (`docs/MVP-v0.1.md`) is `lightr run/snapshot/hydrate/status`, local-only,
native engine, built on clw pipelines. The clw pipelines are generic over
`CasTransport + AcTransport` (verified 2026-06-11 against
`corelink-workspaces` @ `f8f5edf`), so the only genuinely new substance is a
local filesystem transport + the CLI surface.

## Decision

```
hugr-lightr/
  Cargo.toml                 # workspace (lead-owned; agents never touch)
  crates/
    lightr-store/              # LocalStore: CasTransport + AcTransport over disk (ADR-0003)
    lightr-cli/                # bin `lightr`: snapshot/hydrate/status/run over clw pipelines
    lightr-acceptance/         # assert_cmd end-to-end suite (A1–A7 in the build spec)
```

- Dependency rule: `lightr-cli → lightr-store`; `lightr-store` depends only on
  clw crates + std needs; `lightr-acceptance` invokes the compiled `lightr`
  binary (no library dependency on either crate).
- Binary name `lightr`; crate names are workspace-internal (`lightr-store`,
  `lightr-cli`). Any future crates.io publication uses the `hugr-lightr` package
  name if `lightr` is unavailable (verify on crates.io/brew before publishing) — not exercised in v0.1 (`publish = false`).
- No `Engine` trait crate in v0.1 — see ADR-0005.

## Consequences

- Three disjoint write-scopes → the v0.1 wave parallelizes conflict-free.
- The acceptance crate exercising only the binary keeps it honest as a
  product test (no internal shortcuts) and decouples it from refactors.
