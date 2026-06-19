//! A9–A16 per build-spec-r1.md §5 — authored by WP-R1-W5 (red-first).
//!
//! Amendment (lead): A13 drops the "memo HIT" assertion; assert only that bisect
//! finds the correct flip index (== 1, see spec §5 authoring law).
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo check -p lightr-acceptance --all-targets.
//! The binary is expected to have todo!() bodies (red-first suite).
//! Do NOT weaken assertions to make them pass against stubs.

#[path = "common/mod.rs"]
mod common;

#[path = "acceptance_r1/helpers.rs"]
mod helpers;

#[path = "acceptance_r1/g1.rs"]
mod g1;

#[path = "acceptance_r1/g2.rs"]
mod g2;

#[path = "acceptance_r1/g3.rs"]
mod g3;

#[path = "acceptance_r1/g4.rs"]
mod g4;

#[path = "acceptance_r1/g5.rs"]
mod g5;

#[path = "acceptance_r1/g6.rs"]
mod g6;
