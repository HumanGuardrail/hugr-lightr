//! WP-DF-IMGCFG: pure-metadata config-record instruction bodies, split from
//! `exec_instr.rs` to keep that file under the 400-line godfile cap. Each writes
//! one field of the image config sidecar (`ImageConfig`) and mutates NO
//! filesystem state (the layer snapshot in `exec.rs` persists the sidecar).
//!
//! Re-exported from `exec_instr` so `exec.rs` calls them as `exec_instr::*`.
use lightr_core::Result;

use super::{interp_vec, BuildCtx, ImageConfig};
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
