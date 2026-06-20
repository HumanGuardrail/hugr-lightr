//! `run` — internal submodules for lightr-run's core logic.
//! All public items are re-exported at the crate root via `lib.rs`.

mod ac;
mod apply_cfg;
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
pub mod respawn;
pub mod spawn;
pub mod stop;
pub mod supervise;
pub mod supervise_native;
mod svz;
pub mod types;
pub mod vzmemo;

#[cfg(test)]
mod tests;
