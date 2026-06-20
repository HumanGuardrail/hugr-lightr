//! WP-DF-IMGCFG consume-side tests: the engine path (`lightr run --rootfs
//! <image>`) HONORS the image config with Docker precedence (CLI > image).
//!
//! These pin the two pure composition rules the handler applies before handing
//! the spec to an engine — argv (entrypoint/cmd via `effective_argv`) and the
//! cwd (workdir, CLI-over-image). They never spawn an engine, so they are
//! parallel-safe and platform-independent.
use super::resolve_run_cwd;
use lightr_build::{effective_argv, ImageConfig};
use std::path::{Path, PathBuf};

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
