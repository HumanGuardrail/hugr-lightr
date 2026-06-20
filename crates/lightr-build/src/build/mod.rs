//! Build submodule -- re-exports the public build API.
pub mod compose;
pub(crate) mod exec;
// R-IMGCFG (parity-contract.md §0): ImageConfig sidecar + shared effective_argv.
pub mod imgcfg;
pub(crate) mod memo;
pub(crate) mod parse;
// R-VARENG (parity-contract.md §0): frozen interpolate() signature + VarScope.
// The engine is WP-DF-02; compose consumes this fn directly (LEAD DECISION).
pub mod vars;

pub use compose::{
    compose_down, compose_supervise, compose_up, parse_compose, Compose, ComposeHandle, Service,
    ServiceSpec, StackSpec,
};
pub use exec::{build, step_reads_clock_or_net, BuildReport};
pub use imgcfg::{effective_argv, ImageConfig};
pub use parse::{
    parse_dockerfile, parse_dockerfile_full, BuildStep, CmdForm, Directives, Healthcheck,
    HealthcheckOpts, Instr,
};
pub use vars::{interpolate, VarScope};
