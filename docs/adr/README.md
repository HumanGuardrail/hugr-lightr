# ADR index — hugr-lightr

Status flow: **Proposed → Accepted** (owner closes) → Superseded if
replaced. Code is written only against Accepted ADRs. ADRs 0009–0016 and
the batch acceptance of 0001/0002/0004/0006/0007 were Accepted under the
**owner overnight mandate of 2026-06-11** (verbatim in
`../decisions-log.md`) — each remains subject to morning review.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-workspace-architecture.md) | Workspace & crate architecture | Accepted (mandate) — crate set restated by build-spec v2 |
| [0002](0002-clw-seam.md) | clw seam: path-dependencies | Accepted (mandate) — narrowed to bridge crates by 0011 |
| [0003](0003-local-store.md) | LocalStore (clw-pipeline store) | **Superseded by 0009** |
| [0004](0004-ref-grammar.md) | Ref grammar `@namespace/name` | Accepted (mandate) |
| [0005](0005-engine-posture.md) | v0.1 native-only, no Engine trait | **Superseded by 0014** |
| [0006](0006-toolchain.md) | Toolchain & edition | Accepted (mandate) |
| [0007](0007-telemetry.md) | Zero telemetry | Accepted (mandate) |
| [0008](0008-license.md) | License | **Accepted — Apache-2.0** (LICENSE gate lifted; GTM/M1 timing remains) |
| [0009](0009-content-plane.md) | The content plane (file CAS + CoW + refs) | Accepted (mandate) |
| [0010](0010-stat-index.md) | The stat-index | Accepted (mandate) |
| [0011](0011-wire-bridge.md) | Wire bridge (CoreLink/OCI at the border) | Accepted (mandate) |
| [0012](0012-bench-doctrine.md) | Bench doctrine (records as CI gates) | Accepted (mandate) |
| [0013](0013-views.md) | O(1) views + solidifier | Accepted (mandate) — mount layer gated on S1/S3 |
| [0014](0014-vm-states.md) | VM states as refs, boot-never | Accepted (mandate) — gated on S2/S5 |
| [0015](0015-lazy-compose.md) | Lazy compose (socket activation + resume) | Accepted (mandate) |
| [0016](0016-deep-memo.md) | Deep-memo (FS view as tracer) | Accepted (mandate) |
