# How this was built

One human orchestrator, an agent fleet, and a set of gates that don't care who
wrote the code. This page describes the method as git actually records it —
run the commands yourself.

## The shape of the history

```
git log --oneline | wc -l          # 482 commits, 2026-06-11 → 2026-07-01
git log --oneline --merges         # 45 merges
```

Of those merges, 9 are `worktree-agent-*` branches — agents working in
isolated git worktrees, merged back after review. Most of the rest are named
work packets (`wp/r3-build`, `wp/p2-vsock`, `wp/sw-views`, …), each a scoped
contract dispatched to one agent. 8 commits carry GitHub squash-PR suffixes
(`(#N)`) from the later phase, when work moved through PRs on GitHub proper.
One person reviewed, gated, and merged everything; no line reached `main`
without passing the gates below.

## The doctrine

The pipeline, in order, with the artifact each stage leaves behind:

1. **Working-backwards whitepaper** (`docs/whitepaper/hugr-lightr-v2.md`) —
   the finished product described first; everything else derives from it.
2. **ADR-gated decisions** (`docs/adr/`, 19 ADRs) — code is written only
   against Accepted ADRs. A decision that isn't an ADR doesn't exist.
3. **Frozen surface specs** (`docs/spec/build-spec-*.md`) — CLI surfaces,
   acceptance tests, and budgets frozen before implementation starts.
4. **Work packets** — the specs decomposed into scoped contracts, dispatched
   to fleet agents in worktrees, merged only gate-green.
5. **Adversarial audits + a truth ledger** (`docs/spec/parity-audit.md`) —
   every feature maps to the test that proves it or the honest reason it
   isn't proven. Audits are run *against our own claims*.

## The gates (CI, on every PR and on main)

`cargo fmt --check` · `cargo clippy --workspace --all-targets -- -D warnings`
(plus a windows-gnu cross-clippy) · the full workspace test suite · a
**godfile guard**: no source `.rs` file may exceed 400 LOC
(`.github/workflows/ci.yml`). The 400-LOC guard exists because fleet-written
code accretes; the guard forces decomposition instead (see the
`refactor(godfile-split)` commits: `ns.rs` 2266 → 9 modules).

## The audits reversed our own claims — three examples

The truth ledger is only worth something if it moves in both directions.
Reversals it records (all in `docs/spec/parity-audit.md`, F-203/F-CRI-RUN):

- **#95 — the PID namespace was never entered.** `unshare(CLONE_NEWPID)` ran,
  but the engine exec'd without forking, so the workload stayed in the *host*
  pid namespace — false isolation, shipped and "passing". Fixed by forking
  PID 1 into the new namespace (the runc/youki model), then CI-proven
  (`getpid() == 1` inside the container).
- **#101 — the memory cap didn't actually bind.** The 2026-06-26 adversarial
  audit found the earlier "resource limits validated" claim overclaimed: the
  probe had no control, and `memory.max` was written without
  `memory.swap.max`, so the workload spilled to swap unbounded. Fixed
  (`memory.swap.max=0`) and re-proven as a true differential: the same
  allocator is OOM-killed (SIGKILL, exit 137) under `--memory 64m` and
  completes under `--memory 1g`.
- **#102 — we made our own benchmark number worse, on purpose.** lightr's CRI
  backend reported `Running` right after spawning its shim — an undercount
  versus containerd's milestone. Aligning `Running` to the workload's actual
  `execv` (a CLOEXEC exec-success pipe) cost ~22 ms: the published cold-start
  went from 69 ms to the honest 91 ms (`docs/benchmarks/RESULTS.md`, KPI 3).

## What this does and doesn't show

It shows that a gated, spec-first, audit-closed loop lets one person direct a
fleet at high throughput without the usual claim-rot. It does not show
production maturity: the validated scope is exactly what the truth ledger and
the benchmark ledgers (`docs/benchmarks/RESULTS.md`,
`docs/spec/benchmark-results.md`) say, on the hardware they name, and nothing
more.
