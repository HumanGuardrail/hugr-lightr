//! Image config sidecar TYPE + the single shared `effective_argv()` — frozen by
//! the FREEZE-GATE (parity-contract.md §0 R-IMGCFG).
//!
//! The `.lightr-image.json` sidecar (entrypoint/cmd/env/workdir/user/expose/
//! volume/labels/healthcheck/stopsignal/shell/onbuild) and the ONE
//! `effective_argv()` that run + compose + shim all call. The freeze-gate lands
//! the SHAPE + the minimal-correct entrypoint+cmd combination (last-wins); the
//! richer fields are populated by the Dockerfile/run WPs.

use lightr_core::{LightrError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Canonical on-disk filename for the image config sidecar, stored at a layer
/// root. The SAME file the build's `ImageMeta` historically wrote (cmd/env/
/// labels): `ImageConfig` is the GO-FORWARD superset shape, and because every
/// field is `#[serde(default)]` and serde-json ignores unknown keys, the two
/// types round-trip the same file losslessly (back-compat: an old `ImageMeta`-
/// written sidecar loads into `ImageConfig` with the richer fields defaulted,
/// and an `ImageConfig`-written sidecar still exposes `env`/`cmd`/`labels` to a
/// pre-WP `load_meta` reader). The single canonical sidecar filename.
pub const IMAGE_CONFIG_FILE: &str = ".lightr-image.json";

/// The full image config sidecar (Docker `ImageConfig` parity). Every field is
/// `#[serde(default)]` so a partial sidecar (or an older one) round-trips. The
/// freeze-gate freezes the shape; the WPs populate/consume the richer fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageConfig {
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default)]
    pub cmd: Option<Vec<String>>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub expose: Vec<String>,
    #[serde(default)]
    pub volume: Vec<String>,
    #[serde(default)]
    pub labels: Vec<(String, String)>,
    #[serde(default)]
    pub healthcheck: Option<Vec<String>>,
    #[serde(default)]
    pub stop_signal: Option<String>,
    #[serde(default)]
    pub shell: Option<Vec<String>>,
    #[serde(default)]
    pub onbuild: Vec<String>,
}

impl ImageConfig {
    /// Load the `.lightr-image.json` sidecar from a layer `root`. A missing or
    /// unreadable sidecar (e.g. a `scratch`/OCI base without one) is the DEFAULT
    /// config — never an error (Docker: an image without config simply has no
    /// defaults). A malformed sidecar also degrades to the default (best-effort,
    /// matching the historical `load_meta` behaviour this supersedes).
    pub fn load(root: &Path) -> Self {
        match std::fs::read(root.join(IMAGE_CONFIG_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist the config to the `.lightr-image.json` sidecar at `root`. Written
    /// as compact JSON (same shape `load`/`load_meta` read back). A serialize or
    /// write failure is an honest error (fail-closed — a build that cannot record
    /// its config must not silently produce a config-less image).
    pub fn save(&self, root: &Path) -> Result<()> {
        let bytes = serde_json::to_vec(self)
            .map_err(|e| LightrError::InvalidManifest(format!("image config serialize: {e}")))?;
        std::fs::write(root.join(IMAGE_CONFIG_FILE), &bytes).map_err(LightrError::Io)
    }
}

/// Combine an image's `entrypoint`/`cmd` with a caller's command override into
/// the final argv, Docker last-wins semantics:
///
/// - A non-empty `override_cmd` REPLACES the image `cmd` (Docker: `docker run
///   img <args>` overrides CMD, never ENTRYPOINT).
/// - The result is `entrypoint ++ cmd` (the entrypoint is prepended).
/// - If there is no entrypoint, the cmd IS the argv.
///
/// The richer config interactions (`--entrypoint` override, SHELL-form, etc.)
/// are the Dockerfile/run WPs' job; this freezes the canonical combination so
/// run + compose + shim never drift on three copies.
pub fn effective_argv(cfg: &ImageConfig, override_cmd: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    if let Some(ep) = &cfg.entrypoint {
        argv.extend(ep.iter().cloned());
    }
    if !override_cmd.is_empty() {
        // Caller override REPLACES the image CMD (last-wins).
        argv.extend(override_cmd.iter().cloned());
    } else if let Some(cmd) = &cfg.cmd {
        argv.extend(cmd.iter().cloned());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(ep: Option<&[&str]>, cmd: Option<&[&str]>) -> ImageConfig {
        ImageConfig {
            entrypoint: ep.map(|v| v.iter().map(|s| s.to_string()).collect()),
            cmd: cmd.map(|v| v.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn entrypoint_plus_cmd_last_wins() {
        // entrypoint + cmd, no override ⇒ ep ++ cmd
        let c = cfg(
            Some(&["/bin/tini", "--"]),
            Some(&["nginx", "-g", "daemon off;"]),
        );
        assert_eq!(
            effective_argv(&c, &[]),
            vec!["/bin/tini", "--", "nginx", "-g", "daemon off;"]
        );

        // override replaces CMD, keeps entrypoint
        let over = vec!["sh".to_string(), "-c".to_string(), "echo hi".to_string()];
        assert_eq!(
            effective_argv(&c, &over),
            vec!["/bin/tini", "--", "sh", "-c", "echo hi"]
        );

        // no entrypoint ⇒ cmd is the argv
        let c2 = cfg(None, Some(&["echo", "hello"]));
        assert_eq!(effective_argv(&c2, &[]), vec!["echo", "hello"]);

        // no entrypoint, override only
        assert_eq!(effective_argv(&c2, &over), over);
    }
}
