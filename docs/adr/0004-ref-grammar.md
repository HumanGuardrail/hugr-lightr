# ADR-0004 — Ref grammar: `@namespace/name`

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to morning review)
- **Date:** 2026-06-11

One line: a ref is `name` or `@namespace/name`, matching
`^(@[a-z0-9-]+/)?[a-z0-9._-]{1,64}$`; the full string as typed is the
workspace name hashed by clw's `ref_key` — namespaces are opaque in v0.1 and
gain tenant semantics in Stage 2 without a grammar change.

## Context

The grammar is the product's visible surface (`lightr run @hugr/web -- …` is
in every doc and was the approved naming preview). product.md §9 required
freezing it before the first demo. clw's `ref_key(name)` hashes an arbitrary
string with domain separation, so the grammar is purely a CLI-side
validation + convention decision.

## Decision

1. **Grammar:** `^(@[a-z0-9-]+/)?[a-z0-9._-]{1,64}$` — lowercase only; the
   optional leading `@namespace/` reserves team/tenant semantics.
2. **Hashing:** the ref string **as typed** is passed to `ref_key` — no
   normalization beyond grammar validation (reject, don't rewrite).
3. **v0.1 semantics:** all refs resolve against the local store; namespace
   carries no resolution behavior yet. Stage 2 binds `@namespace` to the
   CoreLink tenant without changing what users type.
4. Invalid ref → clear CLI error, exit code 2 (usage error class).

## Consequences

- The demo grammar of every published preview stays valid forever.
- No migration when Stage 2 lands: same strings, added meaning.
