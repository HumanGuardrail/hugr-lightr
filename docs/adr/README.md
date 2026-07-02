# ADR index — hugr-lightr

Status flow: **Proposed → Accepted** (owner closes) → Superseded if
replaced. Code is written only against Accepted ADRs. ADRs 0009–0016 and
the batch acceptance of 0001/0002/0004/0006/0007 were Accepted under the
**owner overnight mandate of 2026-06-11** (verbatim in
`../decisions-log.md`) — all ratified 2026-07-02 under explicit owner delegation (see ../owner-ratify.md).

| ADR | Title | Status |
|---|---|---|
| [0001](0001-workspace-architecture.md) | Workspace & crate architecture | Accepted (ratified 2026-07-02) — crate set restated by build-spec v2 |
| [0002](0002-clw-seam.md) | clw seam: path-dependencies | Accepted (ratified 2026-07-02) — narrowed to bridge crates by 0011 |
| [0003](0003-local-store.md) | LocalStore (clw-pipeline store) | **Superseded by 0009** |
| [0004](0004-ref-grammar.md) | Ref grammar `@namespace/name` | Accepted (ratified 2026-07-02) |
| [0005](0005-engine-posture.md) | v0.1 native-only, no Engine trait | **Superseded by 0014** |
| [0006](0006-toolchain.md) | Toolchain & edition | Accepted (ratified 2026-07-02) |
| [0007](0007-telemetry.md) | Zero telemetry | Accepted (ratified 2026-07-02) |
| [0008](0008-license.md) | License | **Accepted — Apache-2.0** (LICENSE gate lifted; GTM/M1 timing remains) |
| [0009](0009-content-plane.md) | The content plane (file CAS + CoW + refs) | Accepted (ratified 2026-07-02) |
| [0010](0010-stat-index.md) | The stat-index | Accepted (ratified 2026-07-02) |
| [0011](0011-wire-bridge.md) | Wire bridge (CoreLink/OCI at the border) | Accepted (ratified 2026-07-02) |
| [0012](0012-bench-doctrine.md) | Bench doctrine (records as CI gates) | Accepted (ratified 2026-07-02) |
| [0013](0013-views.md) | O(1) views + solidifier | Accepted (ratified 2026-07-02) — mount layer gated on S1/S3 |
| [0014](0014-vm-states.md) | VM states as refs, boot-never | Accepted (ratified 2026-07-02) — gated on S2/S5 |
| [0015](0015-lazy-compose.md) | Lazy compose (socket activation + resume) | Accepted (ratified 2026-07-02) |
| [0016](0016-deep-memo.md) | Deep-memo (FS view as tracer) | Accepted (ratified 2026-07-02) |
