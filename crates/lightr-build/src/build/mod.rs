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
    compose_down, compose_supervise, compose_up, interpolate_compose, parse_compose,
    parse_compose_with_scope, scope_from_project_dir, Compose, ComposeHandle, ComposeSpec,
    EnvScalar, Environment, Healthcheck as ComposeServiceHealthcheck, Service, ServiceDef,
    ServiceSpec, StackSpec, StringOrList,
};
pub use exec::{build, step_reads_clock_or_net, BuildReport};
pub use imgcfg::{effective_argv, ImageConfig};
pub use parse::{
    parse_dockerfile, parse_dockerfile_full, BuildStep, CmdForm, Directives, Healthcheck,
    HealthcheckOpts, Instr,
};
pub use vars::{interpolate, VarScope};
