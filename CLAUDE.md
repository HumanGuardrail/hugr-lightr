# CLAUDE.md

Context for AI agents working in this repo. Keep it lean + high-signal.

## What HuGR Lightr is

**A daemonless, imageless runtime: workspaces materialize from CoreLink's CAS
in seconds, run under the lightest isolation the context permits, and never
re-run what the Action Cache already knows.** The local-first front door of
the CoreLink fabric:

```
HuGR (the company / brand)
 └─ CoreLink (the platform)
     ├─ Cache       — content-addressed CAS + Action Cache     (live)
     ├─ Runners     — ephemeral compute on the cache           (campaign #1)
     ├─ Workspaces  — workspace-as-object (clw client shipped) (campaign #2)
     └─ Lightr        — runtime front door, local-first          (THIS REPO)
   hugit (the forge for agent fleets)                          (campaign #3)
```

**Status (2026-06-11): DESIGN — docs only, no code.** Repo seeded with the
canonical vision; v0.1 scope cut but not started. TechLead is installed
(`.techlead/` state, map at `.techlead/memory/MAP.md`).

Read first: `docs/whitepaper/hugr-lightr-v1.md` (**canonical vision** — source
of truth) · `docs/adr/` (decision records — code is written only against
Accepted ADRs) · `docs/spec/build-spec-v0.1.md` (frozen build surfaces +
acceptance suite + wave partition) · `docs/product/product.md` (ICPs,
pricing posture, open decisions) · `docs/VISION.md` (the funnel) ·
`docs/ARCHITECTURE.md` (engines, seams) · `docs/MVP-v0.1.md` (scope + DoD).

## Principles (decided — don't relitigate without the owner)

1. **No daemon, ever** — nothing runs when nothing runs; `ps` proves it.
2. **No images** — CAS manifests + chunks, lazy by default; OCI is an import
   format, not the model.
3. **Free local, forever, no account** — Stage 1 touches no servers; the
   funnel dies the day the first `lightr run` needs a login.
4. **Isolation à la carte** — `native` = reproducibility, NOT a sandbox
   (stated loudly); hostile tenancy gets `fc` hardware boundaries.
5. **Memoize-first** — the AC check precedes any provisioning.
6. **Pure client of CoreLink** — zero server changes; tenancy/auth/dedup
   semantics are CoreLink's law.
7. **Fail closed** — pinned inputs verified before spawn; no partial
   results; explicit errors over silent cold runs (runners discipline).
8. **Ship the public free tier after Runners M1** — demand must have
   somewhere to convert.
9. **Never charge for the customer's own compute twice** (platform-wide).

## Tense discipline (inherited law — never violate)

- CoreLink dedup is **intra-tenant at GA**; cross-tenant is **staged**
  (`CAP-DEDUP-CROSS-TENANT`). Never claim cross-tenant dedup as live; the
  network-effect moat *depends on it landing* and is stated as such.
- Lightr is **design-phase**: no benchmark, install count, or perf number may
  be claimed as measured. The comparison tables are targets from cited
  precedents (Firecracker, Lambda, OrbStack), not our measurements.

## Relationship to the rest of HuGR

- **Consumes CoreLink Cache** (CAS/AC, tenancy, PAT auth) — pure client,
  like clw; does not fork or modify it.
- **clw (`corelink-workspaces`) is the distribution layer** — snapshot/
  hydrate/memoize pipeline + local L1 cache. Seam form (direct crate dep vs
  transcribed contract) is an open owner decision — `product.md` §9.
- **`Engine` lineage from `corelink-runners`** (`corelink-runner/src/
  isolation.rs`): same spawn/probe/exec/teardown contract, same fail-closed
  lifecycle. In the cloud, Lightr is what a runner lease executes; Runners is
  the fabric, Lightr is the runtime.
- The hugit↔runners integration contract is frozen from hugit's side —
  nothing in this repo may pressure that seam.

⚠️ Sibling repos under `~/Documents/HuGR/` (`corelink-server`,
`corelink-runners`, `corelink-workspaces`, `hugit`, …) frequently have
**other live sessions**. Read-only inspection is fine; **mutation across
repos is not.** Only work on hugr-lightr here.

## Conventions

- English for repo documents; lean, evidence-cited (house style).
- Commits: `Co-Authored-By` trailers; canonical author `gustavo@humangr.com`.
- **Once code exists:** branch → PR → merge, gates green before merge
  (inherit CoreLink/hugit discipline). Rust, exact-pinned deps where the
  house pins (see runners' Cargo conventions).
- Build/refactor waves go through TechLead (`Skill(techlead)`): decompose →
  contract → pack → dispatch → verify. The lead writes doctrine/contracts;
  the fleet implements WPs.
- Crate name `hugr-lightr`, binary `lightr` (verify `lightr` availability on crates.io + brew before any publication — ADR-0008 gate).

## Don't touch

- Other HuGR projects share the parent dir — never mutate siblings.
- `docs/whitepaper/hugr-lightr-v1.md` §9 principles and the tense-discipline
  rules change only with explicit owner approval.
- `.techlead/` is gitignored session state — never commit its contents.
