//! WP-DF-IMGCFG consume-side tests: the engine path (`lightr run --rootfs
//! <image>`) HONORS the image config with Docker precedence (CLI > image).
//!
//! These pin the two pure composition rules the handler applies before handing
//! the spec to an engine — argv (entrypoint/cmd via `effective_argv`) and the
//! cwd (workdir, CLI-over-image). They never spawn an engine, so they are
//! parallel-safe and platform-independent.
use super::{merge_image_env, resolve_run_cwd};
use lightr_build::{effective_argv, ImageConfig};
use std::path::{Path, PathBuf};

fn pairs(kv: &[(&str, &str)]) -> Vec<(String, String)> {
    kv.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn cfg_ep_cmd(ep: Option<&[&str]>, cmd: Option<&[&str]>, workdir: Option<&str>) -> ImageConfig {
    ImageConfig {
        entrypoint: ep.map(|v| v.iter().map(|s| s.to_string()).collect()),
        cmd: cmd.map(|v| v.iter().map(|s| s.to_string()).collect()),
        workdir: workdir.map(String::from),
        ..Default::default()
    }
}

// ── argv: entrypoint + cmd, CLI command overrides image CMD ──────────────────

#[test]
fn run_honors_image_entrypoint_and_cmd() {
    // No CLI command ⇒ argv = entrypoint ++ image CMD (Docker default run).
    let cfg = cfg_ep_cmd(
        Some(&["/bin/tini", "--"]),
        Some(&["server", "-p", "80"]),
        None,
    );
    assert_eq!(
        effective_argv(&cfg, &[]),
        vec!["/bin/tini", "--", "server", "-p", "80"],
    );
}

#[test]
fn cli_command_overrides_image_cmd_keeps_entrypoint() {
    // CLI args REPLACE the image CMD but the image ENTRYPOINT is kept (Docker:
    // `docker run img <args>` overrides CMD, never ENTRYPOINT). Precedence CLI>image.
    let cfg = cfg_ep_cmd(Some(&["/bin/tini", "--"]), Some(&["server"]), None);
    let cli = vec!["sh".to_string(), "-c".to_string(), "echo hi".to_string()];
    assert_eq!(
        effective_argv(&cfg, &cli),
        vec!["/bin/tini", "--", "sh", "-c", "echo hi"],
    );
}

#[test]
fn config_less_image_argv_is_just_the_cli_command() {
    // Behaviour-preserved: an image with the DEFAULT config (no entrypoint/cmd)
    // ⇒ argv == the CLI command, byte-identical to the pre-WP engine path.
    let cfg = ImageConfig::default();
    let cli = vec!["true".to_string()];
    assert_eq!(effective_argv(&cfg, &cli), cli);
}

// ── workdir: CLI -w/--workdir overrides image WORKDIR (CLI > image > fallback) ─

#[test]
fn cli_workdir_overrides_image_workdir() {
    let fallback = Path::new("/host/cwd");
    let cwd = resolve_run_cwd(Some("/cli/wd"), Some("/img/wd"), true, fallback);
    assert_eq!(
        cwd,
        PathBuf::from("/cli/wd"),
        "CLI -w wins over image WORKDIR"
    );
}

#[test]
fn image_workdir_used_when_no_cli_flag() {
    let fallback = Path::new("/host/cwd");
    let cwd = resolve_run_cwd(None, Some("/img/wd"), true, fallback);
    assert_eq!(
        cwd,
        PathBuf::from("/img/wd"),
        "image WORKDIR honored absent CLI flag"
    );
}

#[test]
fn fallback_cwd_when_neither_set() {
    let fallback = Path::new("/host/cwd");
    let cwd = resolve_run_cwd(None, None, true, fallback);
    assert_eq!(cwd, PathBuf::from("/host/cwd"));
}

#[test]
fn rootfs_less_run_always_uses_fallback() {
    // Without a rootfs there is no in-rootfs path, so neither the CLI flag nor a
    // (non-existent) image WORKDIR change the cwd — behaviour-preserved.
    let fallback = Path::new("/host/cwd");
    let cwd = resolve_run_cwd(Some("/cli/wd"), None, false, fallback);
    assert_eq!(cwd, PathBuf::from("/host/cwd"));
}

// ── WP-IMG-ENVUSER: image ENV seeds process env; CLI -e overrides per key ─────

#[test]
fn image_env_seeds_when_no_cli_env() {
    // No CLI -e ⇒ the merge is exactly the image ENV (order preserved).
    let merged = merge_image_env(&pairs(&[("PATH", "/img/bin"), ("LANG", "C")]), &[]);
    assert_eq!(merged, pairs(&[("PATH", "/img/bin"), ("LANG", "C")]));
}

#[test]
fn cli_env_overrides_image_env_per_key() {
    // image ENV < CLI: a CLI key replaces the image value IN PLACE (image order
    // kept); a CLI-only key is appended.
    let merged = merge_image_env(
        &pairs(&[("PATH", "/img/bin"), ("LANG", "C")]),
        &pairs(&[("LANG", "en_US"), ("EXTRA", "1")]),
    );
    assert_eq!(
        merged,
        pairs(&[("PATH", "/img/bin"), ("LANG", "en_US"), ("EXTRA", "1")]),
    );
}

#[test]
fn empty_image_and_cli_env_is_empty_noop() {
    // Config-less image + no -e ⇒ empty overlay ⇒ the engine apply is a no-op
    // (behavior-preserving: child inherits the parent env unchanged).
    assert!(merge_image_env(&[], &[]).is_empty());
}

#[test]
fn cli_only_env_when_image_has_none() {
    let merged = merge_image_env(&[], &pairs(&[("FOO", "bar")]));
    assert_eq!(merged, pairs(&[("FOO", "bar")]));
}

// ── WP-IMG-ENVUSER: image USER, CLI -u overrides (Option::or precedence) ──────
// `run_engine` computes `user.or(cfg.user.as_deref())` — the documented rule:
// CLI -u wins; absent it the image USER applies; absent both ⇒ None (current
// user, behavior-preserving). Pin that precedence directly.

#[test]
fn user_precedence_cli_over_image() {
    let image_user = Some("appuser".to_string());
    let cli: Option<&str> = Some("1000:1000");
    assert_eq!(cli.or(image_user.as_deref()), Some("1000:1000"));
}

#[test]
fn user_precedence_image_when_no_cli() {
    let image_user = Some("appuser".to_string());
    let cli: Option<&str> = None;
    assert_eq!(cli.or(image_user.as_deref()), Some("appuser"));
}

#[test]
fn user_none_when_neither_set() {
    let image_user: Option<String> = None;
    let cli: Option<&str> = None;
    assert_eq!(cli.or(image_user.as_deref()), None);
}
