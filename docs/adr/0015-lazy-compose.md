# ADR-0015 — Lazy compose: socket activation + resume as the default

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to
  morning review)
- **Date:** 2026-06-11

One line: `lightr compose up` starts zero services — it binds per-service
listeners (a few KB each) and **resumes** each service from its suspended
state on the first packet; an idle "running" stack costs ~0 RAM and `up`
returns in milliseconds.

## Context
systemd socket activation and podman quadlets prove the primitive; nobody
ships it as the default dev-compose semantics, and nobody pairs it with
VM-state resume (ADR-0014). Docker/OrbStack/Apple all keep N services
resident.

## Decision
1. compose.yml parsed compatibly (same file devs already have); divergence
   only in runtime semantics: `up` = register, first-connection = resume.
2. Per-stack **ephemeral supervisor** owns the listeners: session-scoped,
   TTL-suicidal, peer-to-peer controllable — consistent with the
   no-resident-daemon law (same class as the compat-socket shim).
3. `--eager` flag restores Docker semantics per service or stack
   (healthcheck-dependent services may need it; documented).
4. Dependency graphs (`depends_on`) resolve on wake cascades, not at `up`.
5. Networking per ADR-0014 §3 + R2 networking parity (DNS names per
   service at OrbStack's bar).

## Consequences
A 16-service stack on an 8 GB laptop. First-packet latency = resume cost
(~100–300 ms vz / ~ms native+CRIU-class later) — visible, honest,
documented; `--eager` exists for the cases that can't tolerate it.
