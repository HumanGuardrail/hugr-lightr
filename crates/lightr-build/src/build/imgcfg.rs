//! Image config sidecar TYPE + the single shared `effective_argv()` — frozen by
//! the FREEZE-GATE (parity-contract.md §0 R-IMGCFG).
//!
//! The `.lightr-image.json` sidecar (entrypoint/cmd/env/workdir/user/expose/
//! volume/labels/healthcheck/stopsignal/shell/onbuild) and the ONE
//! `effective_argv()` that run + compose + shim all call. The freeze-gate lands
//! the SHAPE + the minimal-correct entrypoint+cmd combination (last-wins); the
//! richer fields are populated by the Dockerfile/run WPs.

use serde::{Deserialize, Serialize};

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
