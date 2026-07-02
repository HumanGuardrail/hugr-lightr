## What

<!-- One or two sentences. Link the issue / ADR / parity-audit row. -->

## Why

<!-- The problem this solves. New architectural decisions need an Accepted ADR first. -->

## Gates

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] No source `.rs` file exceeds 400 LOC (godfile guard)
- [ ] New behavior has a test that fails without this change
- [ ] Docs/numbers respect the tense law (measured + cited, or explicit target)
- [ ] `docs/spec/parity-audit.md` updated if a feature row changed
