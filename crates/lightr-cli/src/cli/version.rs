// ──────────────────────────────────────────────────────────────────────────────
// Version string: <pkg> (<git-sha>, <build-date>) — sha/date from build.rs.
// ──────────────────────────────────────────────────────────────────────────────

pub(crate) const LIGHTR_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("LIGHTR_GIT_SHA"),
    ", ",
    env!("LIGHTR_BUILD_DATE"),
    ")"
);

/// Real, copy-pasteable examples shown under `lightr --help`.
pub(crate) const AFTER_HELP: &str = "\
EXAMPLES:
  # Run a command inside a pulled image's rootfs (CoW), memoized
  lightr run --rootfs alpine -- echo hello

  # Snapshot the current directory into the store under a ref
  lightr snapshot --dir . --name @me/proj

  # Import a docker-save tar (or OCI layout) into the store
  lightr oci import ./image.tar --name @docker/myimg

  # Measure the indicator table on this machine, compared to docker
  lightr bench --vs-docker";
