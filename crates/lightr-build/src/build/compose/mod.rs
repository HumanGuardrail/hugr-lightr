//! Compose submodule -- re-exports the public compose API.
pub mod down;
/// CMP-P0-ENVFILE-SVC: per-service `env_file` loader (KEY=VAL fold, lower
/// precedence than the inline `environment` block). Consumed by `lower.rs`.
pub(crate) mod envfile;
pub mod interp;
pub(crate) mod lower;
/// SKELETON-FREEZE: per-aspect lowering STUBS for compose service fields frozen
/// in the model but not yet lowered (depends_on/deploy/networks/restart/...).
/// Each is an honest no-op a feature WP fills. Consumed by `lower.rs`.
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
