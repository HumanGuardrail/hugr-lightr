//! SKELETON-FREEZE: per-aspect lowering for the compose service fields that are
//! FROZEN in the model (`spec.rs`) but only partly lowered into the runtime
//! [`Service`] (`model.rs`) — the dispatcher in `lower.rs` calls every
//! `lower_<aspect>` re-exported here.
//!
//! This file is now a thin FACADE: each `lower_<aspect>` body lives in a
//! cohesive sibling module so a compose-feature WP touching one aspect group is
//! FILE-DISJOINT from a WP touching another (they can fan out without
//! colliding). The dispatcher keeps calling `lower_stubs::lower_<aspect>`
//! unchanged via the re-exports below. The sibling files:
//!  * [`super::lower_runtime`] — `working_dir`/`user`/`restart`/`stop_signal`/
//!    `init`/`tty`/`container_name` (per-process runtime config);
//!  * [`super::lower_resources`] — `deploy` (resources.limits + restart_policy)
//!    plus `cap_add`/`cap_drop`/`privileged` (resource caps + capabilities);
//!  * [`super::lower_net`] — `depends_on`/`networks`/`extra_hosts`/`profiles`
//!    (network attachment + start orchestration);
//!  * [`super::lower_files`] — full-spec `secrets`/`configs` refs plus
//!    `entrypoint`/`stop_grace_period` (file refs + process shape).
//!
//! Each `lower_<aspect>` is either FILLED (the field is parsed and lowered onto
//! `Service`) or an honest no-op STUB: the field is held in [`ServiceDef`] but
//! the runtime `Service` carries no slot for it yet, so the current behavior is
//! "ignored" — and these stubs reproduce exactly that (byte-identical
//! `Service`). A future compose-feature WP fills EXACTLY ONE stub body (and
//! widens `model.rs` for its target field), touching no sibling aspect and not
//! colliding on `lower.rs` beyond its already-present call site.
//!
//! Convention for filling a stub:
//!  1. add the target field(s) to the runtime `Service` in `model.rs` (+
//!     `empty_service`), via that field's own WP;
//!  2. replace the body in the owning sibling file with the lowering, reading
//!     from `def` and writing onto `svc`;
//!  3. add tests to the relevant `*_tests.rs`.
//!
//! The `_` bindings in the stubs are deliberate: they document that the source
//! field exists and is intentionally not yet consumed (no `#[allow(unused)]`, no
//! debt).

// Re-export every per-aspect helper so `lower.rs` (the dispatcher) calls
// `lower_stubs::lower_<aspect>` unchanged, and so the `#[cfg(test)]` module
// below sees them through `super::*`.
pub(super) use super::lower_files::{
    lower_entrypoint, lower_spec_configs, lower_spec_secrets, lower_stop_grace_period,
};
pub(super) use super::lower_net::{
    lower_depends_on, lower_extra_hosts, lower_networks, lower_profiles,
};
pub(super) use super::lower_resources::{
    lower_cap_add, lower_cap_drop, lower_deploy, lower_privileged,
};
pub(super) use super::lower_runtime::{
    lower_container_name, lower_init, lower_restart, lower_stop_signal, lower_tty, lower_user,
    lower_working_dir,
};

// Re-exported for the `#[cfg(test)]` module below (its `super::*` resolves these
// type names exactly as before the split).
#[cfg(test)]
use super::model::Service;
#[cfg(test)]
use super::spec::ServiceDef;

// CMP-P1-DEPLOY tests live in a sibling file (godfile headroom). House
// convention — see network_tests.rs / imgmeta_tests.rs.
#[cfg(test)]
#[path = "lower_stubs_tests.rs"]
mod tests;
