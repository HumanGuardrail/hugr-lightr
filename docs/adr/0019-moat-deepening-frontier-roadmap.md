# ADR-0019 — Moat-deepening frontier roadmap (and why CXL is out of scope)

- **Status:** Proposed — roadmap only. **Not a go-live blocker.** Records direction so frontier bets are not lost or chased prematurely.
- **Date:** 2026-06-19
- **Context owner:** founder (north-star call); tech-lead (technical scoping).

## Context

North star: *demolish Docker on every runtime indicator and hold an indisputable,
compounding moat.* The recurring question is which frontier technologies deepen
that moat — and specifically whether **CXL / Memory-as-a-Service** belongs on the
Lightr roadmap.

## Decision

### 1. The moat is the structural runtime model — deepen *that*, go deep not broad
Lightr's moat is already structural and **compounding**, which is what makes it
indisputable (a competitor cannot retrofit it without abandoning their model):

- **CAS + content-dedup** — the more the fleet runs, the less each node stores/pulls.
- **Shared Action Cache network effect (CoreLink)** — one node's build is another
  node's instant HIT. Docker's image model has no equivalent; matching it means a
  rewrite. This is the network-effect moat (staged as cross-tenant dedup in the
  whitepaper).
- **Daemonless / imageless** — structural, not a toggle Docker can flip.

A moat is won by going *absurdly deep* on these few axes, not by spreading across
every trend.

### 2. Three runtime-layer frontiers worth exploring (Docker structurally can't match)
Post-go-live roadmap, in priority order:

1. **VM snapshot / restore → sub-ms resume.** Precedents: Firecracker snapshots,
   AWS Lambda SnapStart. Docker has no answer to "container resumes from nothing
   instantly." Compounds the cold-start humiliation.
2. **Distributed + predictive memoization.** "Never run anything twice, anywhere."
   Lightr's memo/replay signature × the shared-AC network effect. Predictive
   prefetch of likely-needed objects.
3. **Confidential computing (SEV-SNP / TDX).** Hardware-isolated containers —
   Docker lacks this natively. Opens the hostile-tenancy market.

All three are **runtime-layer** and **compound the existing moat**.

### 3. CXL / Memory-as-a-Service — OUT of scope for Lightr
- **Wrong layer:** datacenter hardware fabric (memory disaggregation over PCIe),
  *below* the OS. Lightr is a userspace runtime *above* it; there is no contact point.
- **Wrong problem:** CXL/MaaS addresses GPU inference memory pressure. Lightr's
  bottleneck is disk/CAS (lazy materialization, CoW), not pooled RAM.
- **Wrong thesis:** CXL is rack-scale datacenter fabric; Lightr is **local-first**
  (runs on the developer's laptop, zero servers). Adopting it pulls against the
  very property that makes Lightr the obvious Docker replacement.
- **Where it could matter for HuGR (not Lightr):** infrastructure for **CoreLink
  Runners** *if* they ever run GPU inference at datacenter scale — a different repo,
  team, and timeline (post-Runners-M1). Not a Lightr decision.

## Consequences
- The three frontiers above are tracked here as deliberate post-go-live bets; none
  blocks the current launch (which ships the already-structural Docker humiliation).
- CXL is explicitly parked: revisit only inside a CoreLink Runners GPU-serving
  context, never as a Lightr runtime concern.
- Tense discipline: every comparative above is a **target from cited precedent**,
  not a measured Lightr result.
