//! A1–A8 per build-spec v2 §8 — authored by WP-6.
//! Every test: LIGHTR_HOME → per-test tempdir; never touches ~.
//!
//! Gate: cargo check -p lightr-acceptance --all-targets must pass.
//! The binary is expected to have todo!() bodies (red-first suite).
//! Do NOT weaken assertions to make them pass against stubs.

#[path = "common/mod.rs"]
mod common;

#[path = "acceptance/helpers.rs"]
mod helpers;

#[path = "acceptance/group1.rs"]
mod group1;

#[path = "acceptance/group2.rs"]
mod group2;
