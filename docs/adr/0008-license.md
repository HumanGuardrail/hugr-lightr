# ADR-0008 — License

- **Status:** Open (owner decision — not closable by the lead)
- **Date:** 2026-06-11

One line: undecided between permissive (MIT/Apache-2.0, maximum adoption)
and source-available (BSL-style, hyperscaler protection); **not blocking
v0.1 code** because the workspace ships `license = "UNLICENSED"` +
`publish = false` (the clw precedent) until this closes.

## Context

The funnel requires the local tool to be *free*; free ≠ open source. The
options and trade-offs are laid out in `docs/product/product.md` §9.1.
Distribution (brew tap, public repo) is the forcing function, and it is
gated behind Runners M1 anyway (whitepaper §9.8).

## Decision

Deferred to the owner. Hard gate recorded: **no public distribution of any
artifact from this repo until this ADR is Accepted** with a concrete
license. The repo remains private/unpublished meanwhile.

## Consequences

- v0.1 development proceeds unblocked.
- The release checklist (when it exists) carries this gate explicitly.
