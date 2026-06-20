//! SKELETON-FREEZE: per-aspect lowering STUBS for the compose service fields
//! that are FROZEN in the model (`spec.rs`) but NOT yet lowered into the runtime
//! [`Service`] (`model.rs`).
//!
//! Each `lower_<aspect>` here is an honest no-op: the field is parsed and held in
//! the [`ServiceDef`], but the runtime `Service` type carries no slot for it yet,
//! so the current behavior is "ignored" — and these stubs reproduce exactly that
//! (byte-identical `Service`). A future compose-feature WP fills EXACTLY ONE of
//! these bodies (and widens `model.rs` for its target field), touching no sibling
//! aspect and not colliding on `lower.rs` beyond its already-present call site.
//!
//! Why a separate module: it keeps `lower.rs` (the dispatcher + the aspects that
//! ALREADY lower) under the godfile limit and gives every not-yet-lowered aspect
//! its own self-contained editable function.
//!
//! Convention for filling a stub:
//!  1. add the target field(s) to the runtime `Service` in `model.rs` (+
//!     `empty_service`), via that field's own WP;
//!  2. replace the body here with the lowering, reading from `def` and writing
//!     onto `svc`;
//!  3. add tests to `lower_tests.rs`.
//!
//! The `_` bindings below are deliberate: they document that the source field
//! exists and is intentionally not yet consumed (no `#[allow(unused)]`, no debt).
use super::model::Service;
use super::spec::ServiceDef;

/// `depends_on` (CMP-P0-DEPENDS): startup ordering / health-gated dependencies.
/// Stub — start order is not modeled in the runtime `Service` yet.
pub(super) fn lower_depends_on(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.depends_on;
}

/// `deploy` (CMP-P1-DEPLOY-RES): replicas + resource limits + restart policy.
/// Stub — no resource/replica slot in the runtime `Service` yet.
pub(super) fn lower_deploy(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.deploy;
}

/// `networks` (CMP-P1-NETWORKS): service network attachments + aliases.
/// Stub — Lightr publishes on loopback today; no per-network model yet.
pub(super) fn lower_networks(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.networks;
}

/// `restart` (top-level restart policy string, e.g. `always`/`on-failure`).
/// Stub — the supervisor restart policy is not driven from here yet.
pub(super) fn lower_restart(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.restart;
}

/// `secrets` (full compose-spec form: refs into the top-level `secrets:` block).
/// Stub — distinct from the lowered Lightr `name=ref` extension (`lower_secrets`
/// in `lower.rs`); the rich source/target grammar is a later WP.
pub(super) fn lower_spec_secrets(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.secrets;
}

/// `configs` (full compose-spec form: refs into the top-level `configs:` block).
/// Stub — counterpart of [`lower_spec_secrets`]; the Lightr `name=ref` form is
/// lowered by `lower_configs` in `lower.rs`.
pub(super) fn lower_spec_configs(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.configs;
}

/// `extra_hosts`: additional `/etc/hosts` entries. Stub — not injected yet.
pub(super) fn lower_extra_hosts(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.extra_hosts;
}

/// `stop_grace_period`: graceful-stop window before SIGKILL. Stub — the
/// teardown path uses a fixed grace today.
pub(super) fn lower_stop_grace_period(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.stop_grace_period;
}

/// `stop_signal`: the signal used to stop the container. Stub — teardown sends
/// the default signal today.
pub(super) fn lower_stop_signal(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.stop_signal;
}

/// `init`: run a PID-1 reaper inside the container. Stub — not wired yet.
pub(super) fn lower_init(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.init;
}

/// `tty`: allocate a TTY. Stub — not wired yet.
pub(super) fn lower_tty(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.tty;
}

/// `cap_add`: Linux capabilities to add. Stub — capability set not modeled yet.
pub(super) fn lower_cap_add(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.cap_add;
}

/// `cap_drop`: Linux capabilities to drop. Stub — counterpart of
/// [`lower_cap_add`].
pub(super) fn lower_cap_drop(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.cap_drop;
}

/// `privileged`: run the container in privileged mode. Stub — not honored yet.
pub(super) fn lower_privileged(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.privileged;
}

/// `container_name`: explicit container name override. Stub — Lightr derives the
/// runtime name from the project + service today.
pub(super) fn lower_container_name(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.container_name;
}

/// `working_dir`: process working directory. Stub — not set on the runtime
/// `Service` yet.
pub(super) fn lower_working_dir(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.working_dir;
}

/// `user`: run-as user/uid. Stub — not set on the runtime `Service` yet.
pub(super) fn lower_user(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.user;
}

/// `entrypoint`: override the image entrypoint. Stub — only `command` is lowered
/// today (`lower_command` in `lower.rs`); the runtime `Service` has no separate
/// entrypoint slot yet.
pub(super) fn lower_entrypoint(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.entrypoint;
}
