# Contributing to HuGR Lightr

## Toolchain

`rust-toolchain.toml` pins **Rust 1.96.0** — rustup installs it on demand.
No other toolchain is supported for CI-relevant work.

## Build & test

```
cargo build --release            # single binary: target/release/lightr
cargo build --release --features vz   # + the macOS Virtualization.framework engine
cargo build --workspace          # debug bin (acceptance tests exec target/debug/lightr)
cargo test --workspace           # full suite
```

## The gates (must be green before merge)

Run locally with `scripts/gate.sh`, or individually:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI (`.github/workflows/ci.yml`) additionally enforces:

- **Godfile guard:** no source `.rs` file may exceed **400 LOC**
  (examples/tests directories excluded — see the workflow for the exact
  filter). If your change pushes a file over, split it into cohesive
  modules; don't ask for an exemption.
- **Windows cross-clippy** (`--target x86_64-pc-windows-gnu -D warnings`) —
  catches cfg-gated dead code on the platform you didn't build.

## The ADR rule

Code is written **only against Accepted ADRs** (`docs/adr/`). If your change
embodies a new architectural decision — a new engine, seam, on-disk format,
dependency posture — write the ADR first and get it Accepted. A PR that
smuggles in an undecided decision will be closed, however good the code.

## The tense law (for any doc, comment, or commit that states a number)

A performance/footprint/benchmark number may be stated **only** if it was
actually measured, and it must carry its run context (named hardware or the
public CI runner) plus a reproduce path. The measured ledgers are
`docs/benchmarks/RESULTS.md` (Linux, CI) and `docs/spec/benchmark-results.md`
(macOS, Intel box). Anything not yet measured is an explicit **target**,
cited to its precedent — never phrased as a measurement. Absent competitors
in comparisons print SKIP, never a fabricated number.

## Fail-closed

Unsupported paths return an honest error; they never silently degrade,
no-op, or fabricate success. If you can't enforce a flag on an engine,
error — don't ignore it. `docs/spec/parity-audit.md` is the truth ledger;
if your change affects a row, update the row in the same PR.

## PR expectations

- Branch → PR → merge; gates green before merge. Keep PRs scoped to one
  concern.
- Say **what** changed and **why** (link the ADR/issue/parity row).
- New behavior comes with a test that fails without the change.
- English, lean, evidence-cited. Commits use `Co-Authored-By` trailers where
  applicable.
