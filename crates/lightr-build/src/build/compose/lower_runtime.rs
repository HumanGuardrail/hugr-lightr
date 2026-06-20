//! SKELETON-FREEZE (per-aspect, runtime-config group): lowering for the compose
//! service fields that shape the per-process RUNTIME of the container —
//! `working_dir`/`user`/`restart`/`stop_signal`/`init`/`tty`/`container_name`.
//!
//! Each `lower_<aspect>` here either lowers its field onto the runtime
//! [`Service`] (filled) or is an honest no-op (stub) for a field that is frozen
//! in the model but not yet carried by `Service`. A future compose-feature WP
//! fills EXACTLY ONE stub body (and widens `model.rs` for its target field),
//! touching no sibling aspect. See `lower_stubs.rs` for the group facade and the
//! stub-filling convention; the `_` bindings document an intentionally-unconsumed
//! source field (no `#[allow(unused)]`, no debt).
use super::model::Service;
use super::spec::ServiceDef;

/// `restart` (top-level restart policy string, e.g. `always`/`on-failure`).
///
/// CMP-LOWER-RUNCFG: copies the compose `restart:` string verbatim onto
/// `svc.restart`; the supervisor threads it into `RunSpec.restart`, honored by
/// the detached re-spawn loop (WP-RC-RESTART). Absent ⇒ `None` ⇒ `no` policy
/// (run once, today's behavior). The policy STRING is transcribed as-is; its
/// parsing/semantics are the run side's law.
///
/// CMP-P1-DEPLOY precedence: a `deploy.restart_policy.condition` WINS over the
/// top-level `restart:`. This defers to a deploy-declared policy (independent of
/// the dispatch order in `lower.rs`): if `deploy.restart_policy.condition` is
/// set, the top-level string is NOT applied.
pub(super) fn lower_restart(def: &ServiceDef, svc: &mut Service) {
    let deploy_sets_restart = def
        .deploy
        .as_ref()
        .and_then(|d| d.restart_policy.as_ref())
        .and_then(|p| p.condition.as_deref())
        .is_some();
    if deploy_sets_restart {
        return;
    }
    svc.restart = def.restart.clone();
}

/// `stop_signal`: the signal used to stop the container. Stub — teardown sends
/// the default signal today.
pub(super) fn lower_stop_signal(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.stop_signal;
}

/// `init`: run a PID-1 reaper inside the container.
///
/// WP-CMP-CONFIG-LOWER: copies the compose `init:` bool onto `svc.init`; the
/// supervisor threads it into `RunSpec.init` (RC-SEAM). Absent ⇒ `false` ⇒ no
/// init process (today's behavior).
pub(super) fn lower_init(def: &ServiceDef, svc: &mut Service) {
    svc.init = def.init.unwrap_or(false);
}

/// `tty`: allocate a TTY.
///
/// WP-CMP-CONFIG-LOWER: copies the compose `tty:` bool onto `svc.tty`; the
/// supervisor threads it into `RunSpec.tty` (RC-SEAM). Absent ⇒ `false` ⇒ no
/// TTY (today's behavior).
pub(super) fn lower_tty(def: &ServiceDef, svc: &mut Service) {
    svc.tty = def.tty.unwrap_or(false);
}

/// `container_name`: explicit container name override.
///
/// WP-CMP-CONFIG-LOWER: copies the compose `container_name:` string onto
/// `svc.container_name`; the supervisor uses it as the run-dir name at the spawn
/// site (`prepare_service_cwd`). Absent ⇒ `None` ⇒ Lightr derives the run name
/// from the service name (today's behavior). The compose service NAME
/// (depends_on edges, discovery keys, peer matching) is unchanged — only the
/// materialized run dir is renamed.
pub(super) fn lower_container_name(def: &ServiceDef, svc: &mut Service) {
    svc.container_name = def.container_name.clone();
}

/// `working_dir`: process working directory.
///
/// CMP-LOWER-RUNCFG: copies the compose `working_dir:` string onto
/// `svc.working_dir`; the supervisor threads it into `RunSpec.workdir`
/// (WP-RC-WORKDIR — resolved against the service cwd). Absent ⇒ `None` ⇒ run in
/// the service cwd (today's behavior).
pub(super) fn lower_working_dir(def: &ServiceDef, svc: &mut Service) {
    svc.working_dir = def.working_dir.clone();
}

/// `user`: run-as user/uid.
///
/// CMP-LOWER-RUNCFG: copies the compose `user:` string onto `svc.user`; the
/// supervisor threads it into `RunSpec.user` (WP-RC-USER — `uid[:gid]` or
/// `name[:group]`, applied cfg(unix) before exec). Absent ⇒ `None` ⇒ run as the
/// current user (today's behavior).
pub(super) fn lower_user(def: &ServiceDef, svc: &mut Service) {
    svc.user = def.user.clone();
}
