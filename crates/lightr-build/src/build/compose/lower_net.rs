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
use super::spec::{DependsOn, ServiceDef, ServiceNetworks};

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

/// `networks` (WP-CMP-NET): the service's network attachments + per-network
/// aliases — the headline "multi-service app talks by name".
///
/// Transcribes the two Docker-faithful shapes onto `svc.networks` as
/// `(network_name, aliases)` in declaration order:
///  * SHORT list (`networks: [frontend, backend]`) ⇒ each name with NO aliases;
///  * LONG map (`networks: {frontend: {aliases: [web]}}`) ⇒ each name with its
///    declared `aliases` (a null attachment value ⇒ empty aliases).
///
/// The names are held UN-prefixed (the lowering has no project name); the
/// supervisor prepends `<project>_` to form the registry network id, matching
/// Docker's per-project network namespacing (`<project>_<network>`).
///
/// Behavior-preserving: a service that declares NO `networks:` lowers to an
/// EMPTY list ⇒ the supervisor spawns it NATIVE with loopback+env discovery,
/// byte-identical to today. A NON-EMPTY list routes the service to the `vz`
/// engine + the shared L2 switch (the hybrid model — only declared-network
/// services attach the switch; plain services stay native). The DNS-by-service-
/// name resolution then comes for free: the service joins the switch under its
/// service name (C9/registry NameTable seeding), so a peer's `curl http://web`
/// resolves automatically.
pub(super) fn lower_networks(def: &ServiceDef, svc: &mut Service) {
    let Some(networks) = &def.networks else {
        return;
    };
    svc.networks = match networks {
        ServiceNetworks::List(names) => names.iter().map(|n| (n.clone(), Vec::new())).collect(),
        ServiceNetworks::Map(map) => map
            .iter()
            .map(|(name, att)| {
                let aliases = att.as_ref().map(|a| a.aliases.clone()).unwrap_or_default();
                (name.clone(), aliases)
            })
            .collect(),
    };
}

/// `extra_hosts`: additional `/etc/hosts` entries (`["host:ip", ...]`).
///
/// WP-CMP-CONFIG-LOWER: HONEST-DEFER. The RC-SEAM RunSpec carries `hostname`
/// (the container's OWN name) but NO host→ip entry-map field for injecting extra
/// `/etc/hosts` lines, and the runtime `Service`/`ServiceSpec` have no slot for
/// it either — so there is nothing to lower onto without widening a non-owned
/// surface (RunSpec lives in `lightr-run`). Kept a no-op (no `/etc/hosts`
/// injection today, behavior-preserving) until a RunSpec extra-hosts field
/// exists; the `_` binding documents the intentionally-unconsumed source field.
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
