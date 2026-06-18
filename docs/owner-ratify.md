# Owner ratification checklist — ADRs

The ADRs below were **Accepted under the owner overnight mandate of
2026-06-11** (`docs/decisions-log.md`, verbatim) so the R0 build could
proceed. Each carries `subject to morning review` in its status line: it was
filed under the overnight mandate and is **pending explicit owner
confirmation**. Before go-live the owner must **ratify** each (promote to a
plain `Accepted`, removing the "mandate / subject to morning review" caveat)
or **revert** it.

Tick the box once the ADR is explicitly ratified by the owner.

## Pending owner ratify/revert (overnight mandate)

- [ ] ADR-0001 Workspace & crate architecture — a three-crate Cargo workspace (`lightr-store` / engine / cli), `publish = false`.
- [ ] ADR-0002 clw seam: path-dependencies — clw consumed via Cargo path-deps; **now narrowed by ADR-0011** (direct path-dep deferred to Stage-2; v0.1 seam is the wire-bridge).
- [ ] ADR-0004 Ref grammar `@namespace/name` — a ref is `name` or `@namespace/name`, matching CoreLink's grammar.
- [ ] ADR-0006 Toolchain & edition — Rust 1.96.0 (machine default), edition 2021 (matching the house).
- [ ] ADR-0007 Telemetry: zero in v0.1 — the binary contains no telemetry, no phone-home, no update check.
- [ ] ADR-0009 The content plane — one content-addressed plane: file-level CAS objects + CoW + refs for everything.
- [ ] ADR-0010 The stat-index — a git-style per-workspace index `path → (size, mtime_ns, …)` for fast change detection.
- [ ] ADR-0011 Wire bridge (CoreLink/OCI at the border) — everything networked lives in quarantined bridge crates; conversion at the border, never on a hot path.
- [ ] ADR-0012 Bench doctrine — `lightr bench` ships inside the binary; the indicator record is a CI gate.
- [ ] ADR-0013 Views: O(1) materialization + solidifier — `hydrate` mounts a view of a manifest (appears in O(1)); a solidifier realizes it.
- [ ] ADR-0014 VM states as refs, boot-never — Linux-on-macOS runs in microVMs the user never watches; VM states are refs (boot-once-per-machine).
- [ ] ADR-0015 Lazy compose — `lightr compose up` starts zero services; per-service socket activation + resume by default.
- [ ] ADR-0016 Deep-memo — beyond whole-`run` memoization, Lightr memoizes inside the run; the filesystem view is the tracer.

## Already resolved — no action required

**Owner-signed (explicit owner decision, not the overnight mandate):**

- ADR-0008 License — **Accepted, Apache-2.0** (owner decision 2026-06-12; LICENSE gate lifted, GTM/M1 timing remains).
- ADR-0017 One product, every desktop (cross-platform engines + portability seams) — **Accepted** (owner mandate 2026-06-12); non-host runtime validation is runbook-gated, not blocking.

**Superseded (replaced by a later ADR; no ratify needed):**

- ADR-0003 LocalStore — **Superseded by ADR-0009** (the content plane).
- ADR-0005 Engine posture (native-only, no Engine trait) — **Superseded by ADR-0014** (engines are plural and real).
