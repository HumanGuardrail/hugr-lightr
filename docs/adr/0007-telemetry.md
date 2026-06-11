# ADR-0007 — Telemetry: zero in v0.1

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to morning review)
- **Date:** 2026-06-11

One line: the v0.1 binary contains **no telemetry, no phone-home, no update
check, no network call of any kind** — Stage-1 "touches no servers" is a
verifiable absolute, and any future telemetry is a separate owner decision
with an opt-in design.

## Context

product.md §9 flagged telemetry as decide-before-v0.1 ("it's in the first
HN thread either way"). The company is named HumanGuardrail; the free tier's
credibility rests on the binary doing exactly what it claims. The acceptance
suite already asserts offline operation.

## Decision

1. Zero network calls in v0.1 unless the user explicitly configures a remote
   (Stage 2 — not in v0.1 scope at all).
2. No usage metrics, no crash reporting, no version check. Diagnostics stay
   local (stderr).
3. Revisit only via a new ADR with owner sign-off; default posture for any
   future design is opt-in + anonymous + documented wire format.

## Consequences

- "Run it with the network unplugged" becomes a marketing-grade,
  test-enforced claim.
- We forgo usage data in v0.1; install counts come from distribution
  channels (brew/GitHub) only.
