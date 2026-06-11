# HuGR Cell — Product Design

Derived from the canonical vision: `docs/whitepaper/hugr-cell-v1.md`.
Status: design phase — every number below is indicative until validated.

## 1. The one-sentence product

A free, daemonless, imageless runtime that makes a Mac dev's containers feel
weightless — and quietly turns every snapshot they take into demand for
CoreLink's cache, Runners and Workspaces.

## 2. The bet (why this exists now)

- Docker Desktop's local tax (always-on VM, 2–4 GB idle) is the most-felt
  dev-tool pain on macOS; OrbStack's traction proves devs switch fast when
  something lighter appears.
- The differentiated substrate (production CAS + Action Cache + clw pipeline
  + fail-closed execution core) already exists across CoreLink repos; Cell is
  the cheap remaining third.
- Agent fleets are exploding demand for ephemeral sandboxes; the team that
  owns the dev's local workspace ref owns where those sandboxes run.
- Cell is the platform's CAC engine: a free tool with standalone value,
  distributed bottom-up (brew/HN), feeding tenants to products that already
  bill (CoreLink Solo exists today at ~$30/mo, ~80% margin documented).

## 3. Who it's for (ICPs) & user stories

### ICP-A — The Mac dev (free user, the funnel's mouth)
"Docker Desktop eats my RAM and my battery." → `brew install hugr-cell`,
`cell run`, no account. Success = Docker Desktop uninstalled. They are not a
buyer; they are distribution and future tenant gravity.

### ICP-B — The team lead / platform engineer (Stage-2 buyer)
"My team re-downloads and re-builds the same things all day." → flips the
CoreLink flag: shared snapshots, team-wide cache hits, onboarding via
`cell hydrate` in seconds. Buys CoreLink tenancy; later evaluates Runners
for CI.

### ICP-C — Agent-fleet teams (Stage-3 demand, via Workspaces)
"I need hundreds of cheap, fast, disposable sandboxes for agents." → the
same refs their devs already use locally, booted as `fc` microVMs on the
Runners fabric. This ICP is shared with corelink-workspaces; Cell is the
on-ramp and the local dev-loop for it.

### ICP-D — HuGR itself (anchor consumer)
hugit and the CoreLink repos dogfood Cell for their own dev loops; the
platform's own CI demand seeds usage data before any external launch.

## 4. The product surface

- **CLI verbs (v0.1):** `cell run <ref|path> -- <cmd>` · `cell snapshot` ·
  `cell hydrate` · `cell status`. Engines arrive as flags/config
  (`--engine native|ns|vz|fc|docker`), defaulting to the lightest safe tier
  for the context.
- **Visible absences are features:** no daemon (`ps` shows nothing between
  runs), no login for Stage 1, no image store to prune, no background
  updater.
- **Ref grammar** (open decision, owner): `@tenant/name` vs
  `tenant/name@version` — it is the product's visible grammar; freeze before
  the first public demo.

## 5. Pricing (indicative — validate before publishing)

**Cell itself: $0, forever, all stages.** Cell has no SKU and no bill. The
money lives upstream, where products already exist:

| Stage | What's billed | Where the SKU lives |
|---|---|---|
| 1 — local | nothing (no server touched, COGS ≈ 0) | — |
| 2 — shared cache | CoreLink tenancy (Solo ~$30/mo exists; team tiers per CoreLink pricing) | corelink-server |
| 3 — compute | Runners concurrency SKUs; Workspaces SKUs | corelink-runners / -workspaces |

Platform law applies: never charge for the customer's own compute twice — a
memoized result is never billed as a run. One bill downstream: a team buying
Workspaces never sees a "Cell" line item.

## 6. Cost & margin model (indicative)

- **Stage 1 COGS ≈ 0** by construction: binary distribution (GitHub
  releases/brew) is the only cost. No free-tier server burn — the classic
  freemium trap is structurally absent.
- **Stage 2 margins are CoreLink's** (~80% documented on Solo), improved by
  Cell users arriving with warm local caches (their first sync uploads less
  than a cold tenant).
- **Dedup tense (inherited law):** intra-tenant at GA — unit economics must
  close on that alone. Cross-tenant dedup (staged, `CAP-DEDUP-CROSS-TENANT`)
  is upside that compounds margin later, not an assumption baked into
  pricing.
- **The flywheel:** more snapshots → higher tenant hit-rates → faster
  product → more usage; lock-in by usefulness (leaving = losing your warm
  cache), not by contract.

## 7. Positioning

| Against | Their story | Cell's axis |
|---|---|---|
| Docker Desktop | the incumbent; VM + daemon + layers | nothing idle, chunk-lazy, daemonless, memoized |
| OrbStack / Apple `container` | lighter VM, same model | different category: distribution + memoization; must still win the day-1 local comparison |
| Podman | daemonless Docker | still image/layer-bound, no cache substrate |
| Nydus/eStargz/SOCI | lazy OCI loading | layer-bound, no cross-artifact chunk dedup, no Action Cache |
| Modal / Fly / Depot | closed internal versions of this machinery | the same machinery as an open, local-first product on a billing cache platform |

## 8. Roadmap (funnel-aligned)

- **v0.1** — Stage-1 wedge: native engine, local-only, macOS arm64
  (`docs/MVP-v0.1.md`; DoD includes "ps shows nothing between runs").
- **v0.2** — `vz` microVMs + OCI import + Linux `ns`: kills the "but I need
  Linux" objection.
- **v0.3** — Stage 2: HuGR account → CoreLink tenant, shared refs, team
  onboarding flow.
- **v1.x** — Stage 3: `fc`, lazy rootfs, snapshot pool, Runners-fabric
  integration. Gate: **ship the public free tier only after Runners M1** so
  demand can convert.

## 9. Open decisions for the owner

1. **License** — MIT/Apache (max adoption) vs BSL (hyperscaler protection).
   The funnel needs *free*, not necessarily *open*; strategic call.
2. **clw seam** — depend on clw crates directly (same org/language, client
   lib by design) vs the transcribed-contract + conformance-vector pattern
   (built for cross-repo *frozen* seams, which this is not yet). Leaning:
   direct dependency.
3. **Ref grammar** — see §4; freeze pre-demo.
4. **Naming collisions** — crates.io `cell` is taken (famous crate): crate
   `hugr-cell`, binary `cell`. Verify brew formula availability before
   announcing.
5. **Telemetry stance** — a guardrail company's free tool should default to
   zero or opt-in anonymous metrics; decide before v0.1, it's in the first
   HN thread either way.
6. **Launch timing vs Runners M1** — principle says after; owner owns the
   exception if GTM pressure says otherwise (logged waiver).
