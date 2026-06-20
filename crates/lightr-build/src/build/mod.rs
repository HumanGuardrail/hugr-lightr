//! Build submodule -- re-exports the public build API.
// WP-DF-08: ARG instruction + --build-arg scoping (crate-internal).
pub(crate) mod args;
pub mod compose;
pub(crate) mod exec;
// Filesystem/CAS helpers split out of exec.rs (behavior-preserving, <400 LOC).
pub(crate) mod exec_fs;
// Per-instruction execution bodies (skeleton-freeze): one `fn` per Dockerfile
// instruction over a shared BuildCtx, so WPs on different instructions stay
// disjoint. `exec::build()` is the thin dispatcher. Behavior-preserving.
pub(crate) mod exec_instr;
// R-IMGCFG (parity-contract.md §0): ImageConfig sidecar + shared effective_argv.
pub mod imgcfg;
pub(crate) mod memo;
pub(crate) mod parse;
// R-VARENG (parity-contract.md §0): frozen interpolate() signature + VarScope.
// The engine is WP-DF-02; compose consumes this fn directly (LEAD DECISION).
pub mod vars;

pub use compose::{
    compose_down, compose_supervise, compose_up, deep_merge, dir_basename, interpolate_compose,
    parse_compose, parse_compose_merged, parse_compose_project_name, parse_compose_with_scope,
    resolve_project_name, sanitize_project_name, scope_from_project_dir, Compose, ComposeHandle,
    ComposeSpec, EnvScalar, Environment, Healthcheck as ComposeServiceHealthcheck, Service,
    ServiceDef, ServiceSpec, StackSpec, StringOrList, DEFAULT_PROJECT, OVERRIDE_FILENAMES,
};
pub use exec::{build, BuildReport};
pub use exec_fs::step_reads_clock_or_net;
pub use imgcfg::{effective_argv, ImageConfig};
pub use parse::{
    parse_dockerfile, parse_dockerfile_full, BuildStep, CmdForm, Directives, Healthcheck,
    HealthcheckOpts, Instr,
};
pub use vars::{interpolate, VarScope};
