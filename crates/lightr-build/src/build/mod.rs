//! Build submodule -- re-exports the public build API.
pub mod compose;
pub(crate) mod exec;
pub(crate) mod memo;
pub(crate) mod parse;

pub use compose::{
    compose_down, compose_supervise, compose_up, parse_compose, Compose, ComposeHandle, Service,
    ServiceSpec, StackSpec,
};
pub use exec::{build, step_reads_clock_or_net, BuildReport};
pub use parse::{parse_dockerfile, BuildStep, Instr};
