//! Compose submodule -- re-exports the public compose API.
/// WP-E: lower a service's `build:` into the runtime `ServiceBuild` (context
/// resolved against the compose file's directory). Consumed by `lower.rs`.
pub(crate) mod build_lower;
/// WP-E: the up-path build step — for a service with a `build:`, run the frozen
/// `build_target` (WP-C) and resolve the produced store ref. Consumed by `up.rs`.
pub(crate) mod build_run;
/// WP-E: the `build:` serde model (`BuildSpec`/`BuildLong`/`BuildArgs`) + its
/// lowered runtime form (`ServiceBuild`). Re-exported via `spec.rs`.
pub(crate) mod build_spec;
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
/// WP-A: the polymorphic value-form enums (`StringOrList`/`ExtraHosts`/
/// `Environment`/`EnvScalar`) split out of `spec.rs` for godfile headroom;
/// re-exported by `spec.rs` so existing imports resolve unchanged.
pub(crate) mod spec_forms;
pub mod supervise;
/// CMP-P0-DEPENDS: `depends_on` topo-order (Kahn) + condition-wait helpers split
/// out of `supervise.rs` for godfile headroom. Consumed by `supervise.rs`.
pub(crate) mod supervise_deps;
/// WP-CMP-NET: the named-networks routing decision (engine + RunSpec network
/// fields) split out of `supervise.rs` for godfile headroom. A service that
/// declares `networks:` routes to the vz engine + the shared L2 switch; a plain
/// service stays native. Consumed by `supervise.rs::start_one_instance`.
pub(crate) mod supervise_net;
/// WP-REPLICAS: `deploy.replicas` planning helpers (instance count, static-port
/// discriminator, per-instance run-name plan) split out of `supervise.rs` for
/// godfile headroom. Consumed by `supervise.rs::start_service_detached`.
pub(crate) mod supervise_replicas;
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
