# ADR-0012 — Bench doctrine: the record as a CI gate

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to
  morning review)
- **Date:** 2026-06-11

One line: `lightr bench` ships **inside the binary** (runs the indicator
table on the user's machine; `--vs-docker` adds the comparison columns when
Docker is present), and the same budgets gate CI — a perf regression is a
red gate with the status of a failing test.

## Context
"Obliterate on every indicator" is unfalsifiable without measurement; the
tense law forbids claiming unmeasured numbers; the bench table is also the
product's best marketing ("don't trust us — run it").

## Decision
1. Budgets (initial set B1–B8, `feature-tree.md`) live in
   `bench/budgets.toml`; CI runs the suite and fails on regression beyond
   noise margins (median-of-N, warmup discipline, machine-class noted).
2. `lightr bench` = same harness, user-facing; prints rung/mode context
   (CoW rung, engine) so numbers are honest per machine.
3. criterion for micro (store/index ops), harness-timed CLI invocations
   for macro (hyperfine methodology, in-house to stay dependency-lean).
4. Spike results (S1–S5) set/adjust budgets before the relevant ring's
   code lands.
5. No public claim outside what this harness measured on named hardware.

## Consequences
The records table in the whitepaper acquires a "measured" column that only
the harness may fill. Marketing and CI share one source of truth.
