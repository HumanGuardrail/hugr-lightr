//! WP-DF-IMGCFG: pure-metadata config-record instruction bodies, split from
//! `exec_instr.rs` to keep that file under the 400-line godfile cap. Each writes
//! one field of the image config sidecar (`ImageConfig`) and mutates NO
//! filesystem state (the layer snapshot in `exec.rs` persists the sidecar).
//!
//! Re-exported from `exec_instr` so `exec.rs` calls them as `exec_instr::*`.
//!
//! SKELETON-FREEZE consolidation: `WORKDIR` and `CMD` (also pure image-config
//! records) joined this file so the per-instruction config edits stay disjoint
//! from the file-placement / build-var groups. Behavior-preserving.
use lightr_core::{LightrError, Result};

use super::{interp_vec, BuildCtx, ImageConfig};
use crate::build::imgcfg::ImageHealthcheck;
use crate::build::parse::{CmdForm, Healthcheck};
use crate::build::vars::interpolate;

/// `ENTRYPOINT`: record the (interpolated) entrypoint argv into the image config
/// (Docker: ENTRYPOINT is the fixed prefix `effective_argv` prepends to CMD/the
/// run override). Recorded as the post-interpolation argv, mirroring `cmd`.
pub(in crate::build) fn entrypoint(ctx: &mut BuildCtx, argv: &[String]) -> Result<()> {
    let argv = interp_vec(argv, ctx.scope, ctx.escape)?;
    let mut cfg = ImageConfig::load(ctx.work_dir);
    cfg.entrypoint = Some(argv);
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `USER`: record the (interpolated) `user[:group]` into the image config
/// (Docker: the image's default run user). `run` honors it unless overridden.
pub(in crate::build) fn user(ctx: &mut BuildCtx, user: &str) -> Result<()> {
    let user = interpolate(user, ctx.scope, ctx.escape)?;
    let mut cfg = ImageConfig::load(ctx.work_dir);
    cfg.user = Some(user);
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `EXPOSE`: record the (interpolated) port specs into the image config (Docker:
/// EXPOSE is metadata — documented ports, NOT an automatic publish). Appended in
/// order, de-duplicated, so multiple EXPOSE lines accumulate (Docker semantics).
pub(in crate::build) fn expose(ctx: &mut BuildCtx, ports: &[String]) -> Result<()> {
    let mut cfg = ImageConfig::load(ctx.work_dir);
    for raw in ports {
        let p = interpolate(raw, ctx.scope, ctx.escape)?;
        if !cfg.expose.contains(&p) {
            cfg.expose.push(p);
        }
    }
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `STOPSIGNAL`: record the (interpolated) stop signal into the image config
/// (Docker: the signal sent to stop the container; consumed by `stop`).
pub(in crate::build) fn stopsignal(ctx: &mut BuildCtx, signal: &str) -> Result<()> {
    let signal = interpolate(signal, ctx.scope, ctx.escape)?;
    let mut cfg = ImageConfig::load(ctx.work_dir);
    cfg.stop_signal = Some(signal);
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `VOLUME`: record the (interpolated) volume mount points into the image config
/// (Docker: declared anonymous-volume paths — metadata). Appended in order,
/// de-duplicated, so multiple VOLUME lines accumulate.
pub(in crate::build) fn volume(ctx: &mut BuildCtx, paths: &[String]) -> Result<()> {
    let mut cfg = ImageConfig::load(ctx.work_dir);
    for raw in paths {
        let p = interpolate(raw, ctx.scope, ctx.escape)?;
        if !cfg.volume.contains(&p) {
            cfg.volume.push(p);
        }
    }
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `WORKDIR`: set the current workdir, ensure it exists in the work dir, AND
/// record it into the image config (Docker: WORKDIR is the container's default
/// cwd, carried in the image config so `run` honors it). The recorded value is
/// the post-interpolation path — the same one used as the build cwd.
pub(in crate::build) fn workdir(ctx: &mut BuildCtx, path: &str) -> Result<()> {
    let path = interpolate(path, ctx.scope, ctx.escape)?;
    *ctx.current_workdir = path.clone();
    let abs = if path.starts_with('/') {
        ctx.work_dir.join(path.trim_start_matches('/'))
    } else {
        ctx.work_dir.join(&path)
    };
    std::fs::create_dir_all(&abs).map_err(LightrError::Io)?;
    let mut cfg = ImageConfig::load(ctx.work_dir);
    cfg.workdir = Some(path);
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `CMD`: record the (interpolated) default argv into the image config.
pub(in crate::build) fn cmd(ctx: &mut BuildCtx, argv: &[String]) -> Result<()> {
    let argv = interp_vec(argv, ctx.scope, ctx.escape)?;
    let mut cfg = ImageConfig::load(ctx.work_dir);
    cfg.cmd = Some(argv);
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `HEALTHCHECK [opts] CMD ...` / `HEALTHCHECK NONE`: record the OCI healthcheck
/// shape into the image config (Docker: the image's default healthcheck, carried
/// in the config so a runtime can probe container liveness).
///
/// - `HEALTHCHECK NONE` → `test = ["NONE"]` (disabled; explicitly drops any
///   inherited healthcheck — Docker parity).
/// - `HEALTHCHECK [opts] CMD <exec-form>` → `test = ["CMD", <argv>...]`.
/// - `HEALTHCHECK [opts] CMD <shell-form>` → `test = ["CMD-SHELL", <command>]`.
///
/// The duration/count opts (`--interval`/`--timeout`/`--start-period`/
/// `--retries`) are recorded as their raw Dockerfile token text (faithful,
/// un-interpreted), mirroring how the parser keeps them. TRANSCRIBE decision:
/// the test command is recorded VERBATIM (no build-time `${VAR}` interpolation)
/// — Docker does not interpolate the HEALTHCHECK command into the image config;
/// `--start-interval` is parsed but not part of the OCI config shape, so it is
/// not recorded here (out of scope).
pub(in crate::build) fn healthcheck(ctx: &mut BuildCtx, check: &Healthcheck) -> Result<()> {
    let hc = match check {
        Healthcheck::None => ImageHealthcheck {
            test: vec!["NONE".to_string()],
            ..Default::default()
        },
        Healthcheck::Cmd { opts, cmd } => {
            let mut test = match cmd {
                CmdForm::Exec(argv) => {
                    let mut t = vec!["CMD".to_string()];
                    t.extend(argv.iter().cloned());
                    t
                }
                CmdForm::Shell(s) => vec!["CMD-SHELL".to_string(), s.clone()],
            };
            test.shrink_to_fit();
            ImageHealthcheck {
                test,
                interval: opts.interval.clone(),
                timeout: opts.timeout.clone(),
                start_period: opts.start_period.clone(),
                retries: opts.retries.clone(),
            }
        }
    };
    let mut cfg = ImageConfig::load(ctx.work_dir);
    cfg.healthcheck = Some(hc);
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `ONBUILD <instruction>`: record the trigger instruction VERBATIM into the
/// image config (Docker: ONBUILD triggers fire when THIS image is used as the
/// base of a derived build). `raw` is the continuation-joined source text of the
/// trigger (the `ONBUILD` keyword already stripped by the caller), appended in
/// order so multiple ONBUILD lines accumulate. Recording is the scope here:
/// trigger-execution on derived builds is a flagged follow-up (not wired).
pub(in crate::build) fn onbuild(ctx: &mut BuildCtx, raw: &str) -> Result<()> {
    let mut cfg = ImageConfig::load(ctx.work_dir);
    cfg.onbuild.push(raw.to_string());
    cfg.save(ctx.work_dir)?;
    Ok(())
}
