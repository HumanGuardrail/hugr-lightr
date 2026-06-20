//! SKELETON-FREEZE (per-aspect, files/process-shape group): lowering for the
//! compose service fields that reference the top-level `secrets:`/`configs:`
//! blocks (full compose-spec form) plus the process-shape aspects `entrypoint`
//! and `stop_grace_period`.
//!
//! Every aspect here is an honest no-op stub: each field is frozen in the model
//! but the runtime `Service` carries no slot for it yet (the Lightr `name=ref`
//! extension for secrets/configs is lowered separately in `lower.rs`). A future
//! compose-feature WP fills EXACTLY ONE stub body (and widens `model.rs` for its
//! target field), touching no sibling aspect. See `lower_stubs.rs` for the group
//! facade and the stub-filling convention; the `_` bindings document an
//! intentionally-unconsumed source field (no `#[allow(unused)]`, no debt).
use super::model::Service;
use super::spec::ServiceDef;

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

/// `stop_grace_period`: graceful-stop window before SIGKILL. Stub — the
/// teardown path uses a fixed grace today.
pub(super) fn lower_stop_grace_period(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.stop_grace_period;
}

/// `entrypoint`: override the image entrypoint. Stub — only `command` is lowered
/// today (`lower_command` in `lower.rs`); the runtime `Service` has no separate
/// entrypoint slot yet.
pub(super) fn lower_entrypoint(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.entrypoint;
}
