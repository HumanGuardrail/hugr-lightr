# ADR-0008 — License

- **Status:** **Accepted — Apache-2.0** (owner decision, 2026-06-12)
- **Date:** 2026-06-11 (proposed) · 2026-06-12 (accepted)

One line: the project is licensed **Apache-2.0** — permissive with an
explicit patent grant, the standard for serious systems infra (Docker,
containerd, Firecracker, Kubernetes); maximizes adoption/funnel, and the
defensible moat lives in CoreLink (the production CAS+AC), not in this
client binary.

## Context

The funnel requires the local tool to be *free*; free ≠ open source. The
options and trade-offs are laid out in `docs/product/product.md` §9.1.
Distribution (brew tap, public repo) is the forcing function, and it is
gated behind Runners M1 anyway (whitepaper §9.8).

## Decision

**Apache-2.0.** Rationale: the funnel wants the free local tool to spread
with zero friction and maximum trust; Apache-2.0 is the trusted-by-default
infra license and its patent grant matters for a systems runtime. A
hyperscaler forking the client gains nothing without CoreLink's production
CAS+AC (which is NOT in this repo and stays proprietary), so the
adoption-vs-protection trade-off resolves toward adoption. `LICENSE`
(full text) + `NOTICE` added; `Cargo.toml` `license = "Apache-2.0"`.

## Consequences

- The **LICENSE gate is lifted**: public distribution is now legally
  permitted. A SEPARATE, still-open gate remains — GTM timing
  (whitepaper §9.8: launch after Runners M1) — so `publish = false` stays
  until that call; the packaging artifacts remain fail-loud until a real
  release URL exists.
- **Owner follow-up (non-blocking):** confirm the exact legal entity for the
  copyright line (`NOTICE` currently says "HumanGuardrail" — add the
  corporate suffix once known). Per-file SPDX headers are optional and not
  added (LICENSE + Cargo.toml SPDX suffice).
