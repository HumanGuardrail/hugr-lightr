# ADR-0006 — Toolchain & edition

- **Status:** Accepted (2026-06-11) (ratified 2026-07-02 under explicit owner delegation to TechLead; basis: implemented + gate-green per docs/spec/parity-audit.md)
- **Date:** 2026-06-11

One line: Rust **1.96.0** (the machine default), edition **2021** (matching
the clw crates we path-depend on), pinned via `rust-toolchain.toml` with a
scaffold-time verification step because of the known rustup-proxy quirk on
this machine.

## Context

- clw pins 1.91.1 / edition 2021 and carries a note: "the rustup proxy is
  broken on the founder Mac — invoke the toolchain directly".
- This machine's default is 1.96.0 (verified working through the proxy on
  2026-06-11); 1.96 compiles edition-2021 crates without issue.
- corelink-runners uses 1.96 / edition 2024.

## Decision

1. `rust-toolchain.toml` pins `channel = "1.96.0"` (installed and default —
   the proxy resolves the default correctly even when channel dispatch is
   flaky).
2. Workspace `edition = "2021"` to stay symmetric with the clw crates the
   workspace compiles against.
3. **Scaffold gate:** the lead verifies `cargo --version` dispatches
   correctly inside the repo before any agent dispatch; if the proxy
   misbehaves, the clw workaround (direct toolchain PATH) is documented in
   the README and the pin file is dropped — decided at scaffold, logged in
   the scaffold commit.

## Consequences

- Reproducible builds on this machine; edition symmetry removes a whole
  class of cross-crate surprise.
- Edition-2024 migration is a deliberate future decision, not a default.
