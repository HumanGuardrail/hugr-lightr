# ADR-0011 — The wire bridge (CoreLink + OCI at the border)

- **Status:** Accepted (owner overnight mandate 2026-06-11 — subject to
  morning review)
- **Date:** 2026-06-11

One line: everything networked — CoreLink sync (Stage 2) and OCI
import/export (R2) — lives in **quarantined bridge crates** (async/tokio
allowed there, forbidden in core); local↔wire conversion (file objects ↔
FastCDC chunk manifests / OCI layers) happens in background at the border,
never on a hot path.

## Context
ADR-0002 chose clw path-deps for the v0.1 core; the perf rework removed
clw from the hot path entirely. The clw crates remain the correct wire
client (FastCDC, BLAKE3, CAS/AC HTTP, conformance with the live server).

## Decision
1. Core crates link zero network code and zero async runtime (F-108).
2. Bridge crates (`lightr-wire`, R4; `lightr-oci`, R2): tokio + clw
   path-deps (ADR-0002 scope narrowed to these crates only).
3. Push: file objects → FastCDC chunk manifests, computed in background;
   pull: chunks → file objects, then normal CoW life.
4. OCI: layers pulled and unpacked **once** into file objects (+ recorded
   provenance); export reverses. Layers never live locally as a runtime
   model.
5. Tense law: dedup intra-tenant at GA; cross-tenant staged.

## Consequences
ADR-0002 amended (narrowed scope), not superseded. Stage-1 binary remains
offline-absolute. The conversion cost is paid once per border crossing, in
background.
