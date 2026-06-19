//! A22–A26 per build-spec-r3.md §6 — authored by WP-R4 (red-first).
//!
//! Gate: cargo fmt --check · cargo clippy -p lightr-acceptance --all-targets
//!       -D warnings · cargo test -p lightr-acceptance.
//!
//! The R3 verbs (build/compose/docker) are merged; every test here is a real,
//! running acceptance test with live assertions (now green). Do NOT weaken assertions.
//!
//! # Native-engine note
//!
//! RUN steps execute via the **native engine** (no filesystem isolation on this
//! box — Intel macOS, `native` is the only available isolation). This means a
//! RUN step CAN write to any absolute path on disk, including paths outside the
//! build context. A22 exploits this: COUNTER_PATH lives in a separate tempdir;
//! the RUN writes to it directly. This is intentional and documented in
//! build-spec-r3.md §2 ("no isolation — stated loudly").
//!
//! # A24 portability caveat
//!
//! The compose lazy test binds on 127.0.0.1 with ports drawn from a high
//! ephemeral range (39000+). On a heavily loaded CI box the ports may already
//! be in use; the test polls up to 2 s for the supervisor to bind and skips the
//! connection-trigger sub-assertion if the port is still unavailable after that
//! window. The core assertions (up fast, 0 services initially, down cleans) are
//! always checked.

#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

#[path = "acceptance_r3/helpers.rs"]
mod helpers;

/// Port-binding lock: all tests in this binary that bind fixed host ports must
/// hold this guard for the duration of the test. Prevents two parallel test
/// threads from fighting over the same port range when the binary is run with
/// the default thread count.
pub(crate) static PORT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[path = "acceptance_r3/group_a22_a23.rs"]
mod group_a22_a23;

#[path = "acceptance_r3/group_a24.rs"]
mod group_a24;

#[path = "acceptance_r3/group_a25.rs"]
mod group_a25;

#[path = "acceptance_r3/group_a26_a308.rs"]
mod group_a26_a308;
