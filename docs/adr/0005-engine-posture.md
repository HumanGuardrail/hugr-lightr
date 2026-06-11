# ADR-0005 — Engine posture v0.1: native-only, no Engine trait yet

- **Status:** Superseded by ADR-0014 (engines are plural and real)
- **Date:** 2026-06-11

One line: v0.1 ships the `native` tier only, and it IS clw-run's process
executor — no `Engine` abstraction is introduced until a second engine
(`vz`, v0.2) exists to justify it.

## Context

The architecture (whitepaper §3) defines five isolation tiers. v0.1's scope
(MVP doc) is native-only: the demonstrated value is reproducibility +
instant hydrate + memoization. clw-run already executes commands as native
processes with memoization. The runners repo owns the `Engine` lineage
(spawn/probe/exec/teardown) for leased, isolated execution.

## Decision

1. v0.1 contains **no Engine trait** and no `lightr-engine` crate.
   `lightr run` = clw-run `run_memoized` (which spawns the process natively).
2. **An abstraction with one implementation is a speculation** — the
   `Engine` trait enters in v0.2 together with the `vz` microVM engine, and
   when it does, it follows the runners contract shape
   (spawn/probe/exec/teardown) rather than inventing a new one.
3. The CLI accepts no `--engine` flag in v0.1 (adding it later is additive,
   not breaking). `native` semantics are stated loudly in `--help` and docs:
   **reproducibility, not a sandbox**.

## Consequences

- v0.1 stays one-sprint-sized; no dead abstraction to maintain.
- v0.2 introduces the trait against two real implementations (native + vz),
  which is when trait shape decisions become informed, not speculative.
