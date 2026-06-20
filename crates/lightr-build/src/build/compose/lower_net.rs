//! SKELETON-FREEZE (per-aspect, networking/orchestration group): lowering for the
//! compose service fields that govern NETWORK attachment and start
//! orchestration — `depends_on` (startup ordering), `networks`, `extra_hosts`,
//! and `profiles` (activation gating).
//!
//! `lower_depends_on` and `lower_profiles` are FILLED (the runtime `Service`
//! carries an edge list + a profile list); `networks`/`extra_hosts` are honest
//! no-op stubs. A future compose-feature WP fills EXACTLY ONE stub body (and
//! widens `model.rs` for its target field), touching no sibling aspect. See
//! `lower_stubs.rs` for the group facade and the stub-filling convention; the
//! `_` bindings document an intentionally-unconsumed source field (no
//! `#[allow(unused)]`, no debt).
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

/// `networks` (CMP-P1-NETWORKS): service network attachments + aliases.
/// Stub — Lightr publishes on loopback today; no per-network model yet.
pub(super) fn lower_networks(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.networks;
}

/// `extra_hosts`: additional `/etc/hosts` entries. Stub — not injected yet.
pub(super) fn lower_extra_hosts(def: &ServiceDef, _svc: &mut Service) {
    let _ = &def.extra_hosts;
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
