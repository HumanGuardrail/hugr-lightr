# Docs map — a reviewer's guide

Recommended reading order. Convention throughout: measured numbers carry
their hardware and a reproduce path; everything else is an explicit
target. When any doc disagrees, the truth ledger (2) is the arbiter.

## Read in this order

1. **[killer-features.md](killer-features.md)** — the three things Docker
   structurally can't do (memoized runs, daemonless, imageless CoW), each
   with its measured table and a one-command demo.
2. **[spec/parity-audit.md](spec/parity-audit.md)** — **the truth
   ledger.** Every feature mapped to its real status, with the test that
   proves it or the honest reason it doesn't; includes the go-live tiers
   and the audits that reversed our own claims.
3. **Benchmarks** — two ledgers, different hardware, no overlap:
   - [benchmarks/RESULTS.md](benchmarks/RESULTS.md) — Linux runtime (`ns`
     cold start, memoization, CRI footprint) on public GitHub-hosted CI.
   - [spec/benchmark-results.md](spec/benchmark-results.md) — macOS
     app-level head-to-head vs docker on a named Intel box.
4. **[adr/](adr/)** — decision records; code is written only against
   Accepted ADRs. Start with 0003 (store), 0005 (engine posture),
   0012 (bench doctrine), 0013 (views).
5. **[METHOD.md](METHOD.md)** — how the repo was built:
   ADR gates, agent fleet, adversarial audits.
6. **[ARCHITECTURE.md](ARCHITECTURE.md)** — execution model, isolation
   tiers, the seams.
7. **[whitepaper/hugr-lightr-v2.md](whitepaper/hugr-lightr-v2.md)** — the
   **working-backwards vision**: written as if the product is complete.
   It is the destination, not the status; for status, see (2).

## Everything else

- [VISION.md](VISION.md) — problem, funnel, economics
- [MVP-v0.1.md](MVP-v0.1.md) — first-slice scope
- [commands.md](commands.md) · [install.md](install.md) ·
  [troubleshooting.md](troubleshooting.md) — user docs
- [RELEASE.md](RELEASE.md) — publish runbook (owner-gated)
- [decisions-log.md](decisions-log.md) — dated decision journal
- [spec/](spec/) — frozen build specs · [product/](product/) — ICPs,
  pricing posture
