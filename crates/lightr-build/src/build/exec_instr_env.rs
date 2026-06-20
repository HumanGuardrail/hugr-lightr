//! SKELETON-FREEZE: `ENV`/`LABEL`/`ARG` instruction bodies (the build-var /
//! label record group), split from `exec_instr.rs` so a WP touching these edits
//! only this file. Behavior-preserving (byte-identical logic to the prior single
//! `exec_instr.rs`); re-exported from `exec_instr` so `exec.rs` calls them as
//! `exec_instr::{env,label,arg}`.
use lightr_core::Result;

use super::{BuildCtx, ImageConfig};
use crate::build::parse::Instr;
use crate::build::vars::interpolate;

/// `ENV`: update the scope + accumulated ENV for all pairs, persisting to meta.
pub(in crate::build) fn env(ctx: &mut BuildCtx, pairs: &[(String, String)]) -> Result<()> {
    // ENV updates the scope for ALL pairs (WP-DF-05 multi-pair).
    // Each value is interpolated against the scope AS IT EVOLVES
    // left-to-right, so a later pair can reference an earlier one in
    // the SAME instruction (Docker semantics). Keys are NOT
    // interpolated (Docker treats ENV/ARG names literally). A
    // single-pair `ENV K v` updates exactly one key, unchanged.
    for (key, raw_val) in pairs {
        let val = interpolate(raw_val, ctx.scope, ctx.escape)?;
        ctx.accumulated_env.retain(|(k, _)| k != key);
        ctx.accumulated_env.push((key.clone(), val.clone()));
        ctx.scope.env.insert(key.clone(), val);
    }
    let mut cfg = ImageConfig::load(ctx.work_dir);
    cfg.env = ctx.accumulated_env.clone();
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `LABEL`: record all (interpolated) pairs into the image config. Labels are
/// not build vars, so they do NOT update the VarScope (Docker semantics).
pub(in crate::build) fn label(ctx: &mut BuildCtx, pairs: &[(String, String)]) -> Result<()> {
    let mut cfg = ImageConfig::load(ctx.work_dir);
    for (key, raw_val) in pairs {
        let val = interpolate(raw_val, ctx.scope, ctx.escape)?;
        cfg.labels.retain(|(k, _)| k != key);
        cfg.labels.push((key.clone(), val));
    }
    cfg.save(ctx.work_dir)?;
    Ok(())
}

/// `ARG`: resolve + bind into the ARG scope (logic in `build::args`).
pub(in crate::build) fn arg(ctx: &mut BuildCtx, instr: &Instr) -> Result<()> {
    ctx.arg_state
        .sync(instr, ctx.arg_overrides, &mut ctx.scope.args);
    Ok(())
}
