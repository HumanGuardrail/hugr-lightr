//! Small pure helpers for the `--rootfs` engine path (WP-DF-IMGCFG /
//! WP-IMG-ENVUSER), split out of `paths.rs` to keep each file under the 400-line
//! godfile cap. `resolve_run_cwd` (image WORKDIR precedence) + `merge_image_env`
//! (image ENV + CLI overlay). Both are pure (no I/O) and re-exported from `paths`.

/// Resolve the engine cwd for a `--rootfs` run, honoring the image WORKDIR with
/// Docker precedence (WP-DF-IMGCFG): the CLI `-w/--workdir` flag wins over the
/// image's recorded WORKDIR; absent both, the caller's `fallback` cwd is used.
/// Only the CLI flag / image WORKDIR take effect WHEN a rootfs is present (the
/// recorded path is in-rootfs); a rootfs-less engine run always keeps `fallback`.
pub(crate) fn resolve_run_cwd(
    cli_workdir: Option<&str>,
    image_workdir: Option<&str>,
    has_rootfs: bool,
    fallback: &std::path::Path,
) -> std::path::PathBuf {
    if !has_rootfs {
        return fallback.to_path_buf();
    }
    match (cli_workdir, image_workdir) {
        (Some(w), _) => std::path::PathBuf::from(w), // CLI > image (Docker precedence)
        (None, Some(w)) => std::path::PathBuf::from(w),
        (None, None) => fallback.to_path_buf(),
    }
}

/// Merge the image's recorded `ENV` with the CLI `-e`/`--env-file` pairs into the
/// final process-env overlay (WP-IMG-ENVUSER), Docker precedence: image ENV is
/// the base, a CLI key with the SAME name OVERRIDES it (image ENV < CLI). Image
/// insertion order is preserved; a CLI-only key is appended. Both empty ⇒ empty
/// (the engine apply is then a no-op — behavior-preserving for a config-less
/// image with no `-e`). Last-write-wins within each source is already resolved
/// upstream (`env::resolve_env_explicit`); here a CLI key simply replaces the
/// image value in place (so the overlay has one entry per key, image-first).
pub(crate) fn merge_image_env(
    image_env: &[(String, String)],
    cli_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = image_env.to_vec();
    for (k, v) in cli_env {
        if let Some(slot) = merged.iter_mut().find(|(mk, _)| mk == k) {
            slot.1 = v.clone(); // CLI overrides the image value (image ENV < CLI)
        } else {
            merged.push((k.clone(), v.clone()));
        }
    }
    merged
}
