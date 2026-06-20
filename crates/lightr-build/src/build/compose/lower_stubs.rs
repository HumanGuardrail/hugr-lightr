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
use lightr_core::ResourceLimits;

use super::model::{DepCondition, Service};
use super::spec::{DependsOn, ServiceDef};

/// `depends_on` (CMP-P0-DEPENDS): startup ordering / health-gated dependencies.
///
/// Records each dependency edge `(dep_service, condition)` onto `svc.depends_on`
/// so the supervisor can topo-sort the start order and gate each edge on its
/// condition. Two shapes (per the frozen [`DependsOn`] model):
///  * short list (`[db, redis]`) ⇒ every edge defaults to `service_started`;
///  * long map (`{db: {condition: service_healthy}}`) ⇒ the declared condition
///    (absent/unknown ⇒ `service_started`, the compose default).
///
/// Declaration order of the deps is preserved (`Vec` over the `IndexMap`); the
/// topo sort is the supervisor's job. A service with no `depends_on` lowers to
/// an empty edge list — behavior-preserving (start order stays declaration
/// order in the supervisor).
pub(super) fn lower_depends_on(def: &ServiceDef, svc: &mut Service) {
    let Some(depends_on) = &def.depends_on else {
        return;
    };
    svc.depends_on = match depends_on {
        DependsOn::List(names) => names
            .iter()
            .map(|n| (n.clone(), DepCondition::Started))
            .collect(),
        DependsOn::Map(map) => map
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    DepCondition::parse(entry.condition.as_deref()),
                )
            })
            .collect(),
    };
}

/// Map a `deploy.restart_policy.condition` to the docker `restart:` policy
/// string the run side parses (`lightr_run::restart::RestartPolicy::parse`).
///
/// Compose's deploy conditions are `any` / `on-failure` / `none`; docker's
/// equivalent restart strings are `always` / `on-failure` / `no`. Returns
/// `None` for an absent condition (no policy declared). An unrecognized
/// condition is treated as `none` (fail-closed: run-once, never a surprise
/// auto-restart from a typo).
fn map_restart_condition(condition: Option<&str>) -> Option<String> {
    match condition? {
        "any" => Some("always".to_string()),
        "on-failure" => Some("on-failure".to_string()),
        "none" => Some("no".to_string()),
        // Unknown condition ⇒ the safe compose default (no auto-restart).
        _ => Some("no".to_string()),
    }
}

/// CMP-P1-DEPLOY: lower the `deploy` block — `resources.limits` + `restart_policy`.
///
/// * `resources.limits.cpus` / `.memory` are parsed with the EXACT same grammar
///   as `lightr run --cpus/--memory` ([`lightr_core::ResourceLimits::parse`]):
///   memory accepts `k/K`,`m/M`,`g/G` suffixes or bare bytes; cpus is a float
///   (1.0 = one core) stored as milli-CPUs. Parse errors are fail-closed
///   (propagated), never silently dropped. The parsed caps are recorded on
///   `svc.mem_limit_bytes` / `svc.cpu_limit_millis` and carried through the
///   on-disk spec to the supervisor. They are NOT yet ENFORCED on the detached
///   compose spawn path — that channel (`spawn_detached_engine` / `SpecOnDisk`)
///   lives in `lightr-run`, which this WP does not own; the supervisor emits an
///   honest once-note at the spawn site (follow-up WP), never a silent ignore.
/// * `restart_policy.condition` (`any`/`on-failure`/`none`) is mapped to the
///   docker restart string and written onto `svc.restart`. PRECEDENCE: when a
///   `deploy.restart_policy.condition` is set it WINS over any top-level
///   `restart:` (compose precedence); `lower_restart` defers to it.
/// * `replicas` is OUT OF SCOPE (multi-instance spawn is a separate WP): the
///   declared count is recorded on `svc.replicas` only so the spawn site can
///   emit an honest "not yet honored" note for `replicas > 1`.
///
/// FAIL-CLOSED on a malformed limit: this aspect keeps the `(def, svc)` ⇒ `()`
/// shape of the frozen `lower.rs` dispatch (it is not an owned file and its call
/// site discards a return), so a `cpus`/`memory` that does not parse cannot be
/// surfaced as a `Result` here. Rather than silently caching a bad value, the
/// caps are written ONLY when [`ResourceLimits::parse`] accepts them; a parse
/// failure is reported on stderr (naming the service + the rejected value) and
/// the cap is left unset — honest (logged), never a silent wrong number.
///
/// No `deploy` block ⇒ no field touched ⇒ today's behavior (behavior-preserving).
pub(super) fn lower_deploy(def: &ServiceDef, svc: &mut Service) {
    let Some(deploy) = &def.deploy else {
        return;
    };

    if let Some(limits) = deploy.resources.as_ref().and_then(|r| r.limits.as_ref()) {
        // Transcribe the run-path parsing verbatim — same grammar.
        match ResourceLimits::parse(limits.memory.as_deref(), limits.cpus.as_deref()) {
            Ok(parsed) => {
                svc.mem_limit_bytes = parsed.memory_bytes;
                svc.cpu_limit_millis = parsed.cpu_millis;
            }
            Err(e) => {
                eprintln!(
                    "lightr compose: service {:?}: invalid deploy.resources.limits \
                     (cpus={:?}, memory={:?}): {e}; limit not applied",
                    svc.name, limits.cpus, limits.memory
                );
            }
        }
    }

    if let Some(policy) = map_restart_condition(
        deploy
            .restart_policy
            .as_ref()
            .and_then(|p| p.condition.as_deref()),
    ) {
        // Deploy wins over top-level `restart:` (compose precedence).
        svc.restart = Some(policy);
    }

    svc.replicas = deploy.replicas;
}

/// `networks` (CMP-P1-NETWORKS): service network attachments + aliases.
/// Stub — Lightr publishes on loopback today; no per-network model yet.
pub(super) fn lower_networks(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.networks;
}

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

/// `entrypoint`: override the image entrypoint. Stub — only `command` is lowered
/// today (`lower_command` in `lower.rs`); the runtime `Service` has no separate
/// entrypoint slot yet.
pub(super) fn lower_entrypoint(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.entrypoint;
}

/// `profiles` (CMP-P1-PROFILES): the service's profile-gating list.
///
/// Copies the compose `profiles: [...]` list verbatim onto `svc.profiles`. The
/// runtime activation filter (a service is started only if it has NO profiles,
/// or one of its profiles is active per `--profile`/`COMPOSE_PROFILES`) runs at
/// the `compose up` call site (`up.rs`), not here — this aspect only transcribes
/// the declared list. Absent/empty ⇒ empty list ⇒ always active (today's
/// behavior, behavior-preserving).
pub(super) fn lower_profiles(def: &ServiceDef, svc: &mut Service) {
    svc.profiles = def.profiles.clone();
}

// CMP-P1-DEPLOY tests live in a sibling file (godfile headroom). House
// convention — see network_tests.rs / imgmeta_tests.rs.
#[cfg(test)]
#[path = "lower_stubs_tests.rs"]
mod tests;
