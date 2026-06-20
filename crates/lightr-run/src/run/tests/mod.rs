//! Test submodules for lightr-run/run. Each file is #[cfg(test)].
//!
//! LIGHTR_HOME is process-global state (all modules compile into the same
//! test binary). A single shared lock here serialises every `isolated_home()`
//! call across all submodules so parallel test threads never stomp on each
//! other's LIGHTR_HOME value.

/// Process-wide env lock: acquired by every `isolated_home()` helper across
/// all sub-modules. Use `ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())`
/// (poison-tolerant) and hold the guard for the lifetime of the test.
pub(super) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

mod deepmemo;
mod memo;
mod memo_key;
mod mount;
mod secrets_tests;
mod spawn_ps;
mod types_tests;
mod vzmemo;
