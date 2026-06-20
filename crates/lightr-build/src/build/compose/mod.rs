//! Compose submodule -- re-exports the public compose API.
pub mod down;
/// CMP-P0-ENVFILE-SVC: per-service `env_file` loader (KEY=VAL fold, lower
/// precedence than the inline `environment` block). Consumed by `lower.rs`.
pub(crate) mod envfile;
pub mod interp;
pub(crate) mod lower;
/// SKELETON-FREEZE per-aspect lowering bodies, grouped cohesively so a feature WP
/// touching one aspect group is FILE-DISJOINT from another. Re-exported through
/// `lower_stubs` (the facade the dispatcher calls). Each holds the
/// secrets/configs (full-spec) refs + entrypoint/stop_grace_period bodies.
mod lower_files;
/// SKELETON-FREEZE per-aspect group: depends_on/networks/extra_hosts/profiles
/// (network attachment + start orchestration). Re-exported via `lower_stubs`.
mod lower_net;
/// SKELETON-FREEZE per-aspect group: deploy (resources.limits + restart_policy)
/// + cap_add/cap_drop/privileged. Re-exported via `lower_stubs`.
mod lower_resources;
/// SKELETON-FREEZE per-aspect group: working_dir/user/restart/stop_signal/init/
/// tty/container_name (per-process runtime config). Re-exported via `lower_stubs`.
mod lower_runtime;
/// SKELETON-FREEZE facade: re-exports every per-aspect `lower_<aspect>` from the
/// `lower_{runtime,resources,net,files}` siblings so the dispatcher (`lower.rs`)
/// calls `lower_stubs::lower_<aspect>` unchanged. Filled bodies + honest no-op
/// stubs; a feature WP fills exactly one stub. Consumed by `lower.rs`.
pub(crate) mod lower_stubs;
/// CMP-P0-MERGE: compose override deep-merge engine + merged parse entry point.
pub mod merge;
pub mod model;
pub mod parse;
/// CMP-P0-PORTS-FULL: the full compose `ports` grammar parser (short + long,
/// ranges, proto, host_ip). Consumed by `lower.rs`.
pub(crate) mod ports;
/// CMP-P1-PROJECT: compose project-name resolution (cli>env>name>basename) +
/// Docker-grammar sanitization. Consumed by the CLI compose handler + `up.rs`.
pub mod project;
pub mod spec;
pub mod supervise;
/// CMP-P0-DEPENDS: `depends_on` topo-order (Kahn) + condition-wait helpers split
/// out of `supervise.rs` for godfile headroom. Consumed by `supervise.rs`.
pub(crate) mod supervise_deps;
pub mod up;

pub use down::compose_down;
pub use interp::{interpolate_compose, scope_from_project_dir};
pub use merge::{deep_merge, parse_compose_merged, OVERRIDE_FILENAMES};
pub use model::{Compose, ComposeHandle, Service, ServiceSpec, StackSpec};
pub use parse::{parse_compose, parse_compose_project_name, parse_compose_with_scope};
pub use project::{dir_basename, resolve_project_name, sanitize_project_name, DEFAULT_PROJECT};
pub use spec::{
    ComposeSpec, DependsOn, DependsOnEntry, Deploy, DeployResources, EnvScalar, Environment,
    Healthcheck, NetworkAttachment, ResourceSpec, RestartPolicy, ServiceDef, ServiceNetworks,
    StringOrList,
};
pub use supervise::compose_supervise;
pub use up::compose_up;
