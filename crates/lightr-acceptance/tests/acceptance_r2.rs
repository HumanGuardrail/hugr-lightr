//! A17–A21 per build-spec-r2.md §5 — authored by WP-R2-W4 (red-first).
//! R2-HARDEN additions: a17b (integrity), a17c (whiteout ordering),
//! a17d (hardlink), A18 strengthened, A21 strengthened.
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo test -p lightr-acceptance.
//! The R2 verbs are merged; these are real, running acceptance tests with
//! live assertions (authored red-first, now green). Do NOT weaken assertions.
//!
//! Fixture form for A17: docker-save TAR. The fixture contains manifest.json
//! plus two uncompressed layer tars (built with the `tar` crate). No sha2 dep
//! is needed: docker-save manifests reference layers by filename, not digest.
//! `flate2` is added as a dev-dep per spec authorisation; layers are kept
//! uncompressed in this fixture so `flate2` is not called directly.
//!
//! For a17b we need a real OCI layout with sha256 digests; sha2 is added as a
//! dev-dep (already authorized in root Cargo.toml workspace.dependencies).

#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "acceptance_r2/helpers.rs"]
mod helpers;

#[path = "acceptance_r2/group_a17.rs"]
mod group_a17;

#[path = "acceptance_r2/group_a18_a19_a20.rs"]
mod group_a18_a19_a20;

#[path = "acceptance_r2/group_a21_a22.rs"]
mod group_a21_a22;
