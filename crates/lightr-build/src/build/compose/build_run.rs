//! WP-E: the up-path build step.
//!
//! For a service that declares a `build:`, run the FROZEN build pipeline
//! (`build_target`, WP-C) over its resolved context + Dockerfile, then resolve
//! the produced image into a store ref the supervisor can hydrate (`image_ref`).
//! This is the one place `up` differs for a `build:` service — everything after
//! it (spec lowering, spawn) is byte-identical to an `image:`-only service.
//!
//! Image-ref semantics (Docker compose `build:` + `image:`):
//!   * `build:` ONLY ⇒ the image is tagged under a derived ref
//!     `<project>_<service>` (Docker uses `<project>-<service>`; a store ref is
//!     an arbitrary string keyed by its hash, so the separator is cosmetic).
//!   * `build:` + `image:` ⇒ build, then tag as the `image:` value (compose's
//!     "build then tag" rule). `image_ref` already holds the `image:` value
//!     (set by `lower_image`), so we build UNDER that ref and leave it.
//!
//! Engine: the NATIVE engine — matching `lightr build`'s default and the
//! repo-wide "native = no filesystem isolation" build posture (RUN executes on
//! the host CoW tree). The build is content-memoized (the Action Cache replays
//! cached layers), so a repeated `up` does not re-execute steps.
use std::path::Path;

use lightr_core::Result;
use lightr_store::Store;

use super::super::build_target;
use super::model::Service;

/// Run the build for one service's `build:` directive and return the store ref
/// the built image is registered under (== the ref the supervisor hydrates).
///
/// `project` namespaces the derived ref for a build-only service so two stacks
/// never collide on the same ref. A service with both `build:` and `image:`
/// builds UNDER the `image:` ref (already in `svc.image_ref`).
pub(crate) fn build_service_image(svc: &Service, store: &Store, project: &str) -> Result<String> {
    let build = svc
        .build
        .as_ref()
        .expect("build_service_image called only for a service with `build:`");

    // The ref to tag the built image under: the explicit `image:` (build-then-tag)
    // when present, else a derived `<project>_<service>` ref (build-only).
    let ref_name = if svc.image_ref.is_empty() {
        derived_ref(project, &svc.name)
    } else {
        svc.image_ref.clone()
    };

    let context = Path::new(&build.context);
    // Docker resolves `dockerfile` relative to the context.
    let dockerfile = context.join(&build.dockerfile);

    build_target(
        context,
        &dockerfile,
        &ref_name,
        lightr_engine::EngineKind::Native,
        store,
        &build.args,
        build.target.as_deref(),
    )?;

    Ok(ref_name)
}

/// Derive the store ref a build-only service's image is tagged under:
/// `<project>_<service>` (Docker's `<project>-<service>` image-name shape; the
/// store ref is hashed, so the separator is cosmetic and `_` keeps it readable).
fn derived_ref(project: &str, service: &str) -> String {
    format!("{project}_{service}")
}

#[cfg(test)]
#[path = "build_run_tests.rs"]
mod tests;
