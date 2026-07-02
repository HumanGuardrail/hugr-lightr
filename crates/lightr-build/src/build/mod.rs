//! Build submodule -- re-exports the public build API.
// WP-DF-08: ARG instruction + --build-arg scoping (crate-internal).
pub(crate) mod args;
pub mod compose;
// WP-DF-IGNORE: `.dockerignore` build-context matcher (crate-internal). Consumed
// by the COPY/ADD executor (exclude from context) + the memo key (exclude from
// the hashed context), so adding an ignored file never busts the cache.
pub(crate) mod dockerignore;
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
// WP-C: `FROM --platform` resolution + validation (crate-internal). Folds the
// resolved platform into the memo key and validates a requested platform
// against the base image's actual (single-arch) platform.
pub(crate) mod platform;
// R-VARENG (parity-contract.md §0): frozen interpolate() signature + VarScope.
// The engine is WP-DF-02; compose consumes this fn directly (LEAD DECISION).
pub mod vars;

// Process-global reader-writer serialization for the `LIGHTR_HOME` env var.
// The var is process-wide across the whole lightr-build test binary. It has two
// classes of test:
//   * WRITERS — tests that `set_var`/`remove_var` it (exec_tests + compose::up_tests)
//     to exercise the memo/action-cache under a custom home. They take the WRITE
//     lock (exclusive) held across set-var → operation → remove/restore.
//   * READERS — tests that call `build()`/`lightr_index::hydrate`/`compose_up`, which
//     internally READ `LIGHTR_HOME` (lightr-index codec + `Store::default_root`) and
//     assume it stays at the default. They take the READ lock (shared) across those
//     calls so they parallelize among themselves but are excluded while a writer holds
//     the lock. Without this, a reader could observe another test's home mid-set-var
//     (⇒ wrong/empty stack dir, e.g. an empty spec.json or `hydrate` NotFound).
// Single shared lock so the serialization is crate-wide, not per-module.
#[cfg(test)]
pub(crate) static LIGHTR_HOME_ENV_LOCK: std::sync::RwLock<()> = std::sync::RwLock::new(());

pub use compose::{
    compose_down, compose_supervise, compose_up, deep_merge, dir_basename, interpolate_compose,
    parse_compose, parse_compose_merged, parse_compose_project_name, parse_compose_with_scope,
    resolve_project_name, sanitize_project_name, scope_from_project_dir, Compose, ComposeHandle,
    ComposeSpec, DependsOn, DependsOnEntry, Deploy, DeployResources, EnvScalar, Environment,
    Healthcheck as ComposeServiceHealthcheck, NetworkAttachment, ResourceSpec, RestartPolicy,
    Service, ServiceDef, ServiceNetworks, ServiceSpec, StackSpec, StringOrList, DEFAULT_PROJECT,
    OVERRIDE_FILENAMES,
};
pub use exec::{build, build_target, BuildReport};
pub use exec_fs::step_reads_clock_or_net;
pub use imgcfg::{effective_argv, ImageConfig, ImageHealthcheck};
pub use parse::{
    parse_dockerfile, parse_dockerfile_full, BuildStep, CmdForm, Directives, Healthcheck,
    HealthcheckOpts, Instr,
};
pub use vars::{interpolate, VarScope};
