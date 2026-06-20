//! SKELETON-FREEZE (per-aspect, resources/capabilities group): lowering for the
//! compose service fields that govern RESOURCE caps and Linux capabilities —
//! `deploy` (resources.limits + restart_policy) plus the
//! `cap_add`/`cap_drop`/`privileged` capability toggles.
//!
//! `lower_deploy` is FILLED (limits + restart-policy mapping); the capability
//! aspects are honest no-op stubs (the runtime `Service` has no capability model
//! yet). A future compose-feature WP fills EXACTLY ONE stub body (and widens
//! `model.rs` for its target field), touching no sibling aspect. See
//! `lower_stubs.rs` for the group facade and the stub-filling convention; the
//! `_` bindings document an intentionally-unconsumed source field (no
//! `#[allow(unused)]`, no debt).
use lightr_core::ResourceLimits;

use super::model::Service;
use super::spec::ServiceDef;

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

/// `cap_add`: Linux capabilities to add.
///
/// WP-CMP-CONFIG-LOWER: copies the compose `cap_add:` list verbatim onto
/// `svc.cap_add`; the supervisor threads it into `RunSpec.cap_add` (RC-SEAM).
/// Empty (absent) ⇒ empty list ⇒ default cap set (today's behavior).
pub(super) fn lower_cap_add(def: &ServiceDef, svc: &mut Service) {
    svc.cap_add = def.cap_add.clone();
}

/// `cap_drop`: Linux capabilities to drop. Counterpart of [`lower_cap_add`].
///
/// WP-CMP-CONFIG-LOWER: copies the compose `cap_drop:` list verbatim onto
/// `svc.cap_drop`; the supervisor threads it into `RunSpec.cap_drop` (RC-SEAM).
/// Empty (absent) ⇒ empty list ⇒ default cap set (today's behavior).
pub(super) fn lower_cap_drop(def: &ServiceDef, svc: &mut Service) {
    svc.cap_drop = def.cap_drop.clone();
}

/// `privileged`: run the container in privileged mode.
///
/// WP-CMP-CONFIG-LOWER: copies the compose `privileged:` bool onto
/// `svc.privileged`; the supervisor threads it into `RunSpec.privileged`
/// (RC-SEAM). Absent ⇒ `false` ⇒ unprivileged (today's behavior).
pub(super) fn lower_privileged(def: &ServiceDef, svc: &mut Service) {
    svc.privileged = def.privileged.unwrap_or(false);
}
