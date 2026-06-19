//! A27–A30 per build-spec-r4.md §6.
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo build -q · cargo test -p lightr-acceptance --test acceptance_r4
//!
//! Dependency notes:
//!   A27 — requires R4-W1 (run --deep-memo flag + honest fallback). BLOCKED until W1 merges.
//!   A28 — requires R4-W2 (lightr schema subcommand). BLOCKED until W2 merges.
//!   A29 — requires R4-W2 (bench B9/B10/B11 indicators). BLOCKED until W2 merges.
//!   A30 — requires R4-W4 (docs/spec/parity-audit.md). BLOCKED until W4 merges.
//!
//! Tests are authored correctly per spec. They will fail (not panic-crash) until
//! the upstream WPs land. Do NOT weaken assertions.
//!
//! # run --json output note
//!
//! `lightr run --json` streams child stdout to stdout and emits a JSON summary
//! to STDERR prefixed `lightr-json: `. A28 parses that line from stderr.

#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "acceptance_r4/helpers.rs"]
mod helpers;

#[path = "acceptance_r4/a27_a28.rs"]
mod a27_a28;

#[path = "acceptance_r4/a29_a30.rs"]
mod a29_a30;
