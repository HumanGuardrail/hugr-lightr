//! `run` — internal submodules for lightr-run's core logic.
//! All public items are re-exported at the crate root via `lib.rs`.

mod ac;
mod ctl;
pub mod deepmemo;
pub mod exec;
pub mod lifecycle;
pub mod logs;
pub mod memo;
pub mod mount;
pub mod paths;
pub mod ps;
pub mod registry;
pub mod spawn;
pub mod stop;
pub mod supervise;
mod svz;
pub mod types;
pub mod vzmemo;

#[cfg(test)]
mod tests;
