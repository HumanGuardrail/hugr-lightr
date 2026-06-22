//! SKELETON-FREEZE: `FROM`/stage instruction body, split from `exec_instr.rs`
//! so a WP touching FROM/stage handling edits only this file. Behavior-preserving
//! (byte-identical logic to the prior single `exec_instr.rs`); re-exported from
//! `exec_instr` so `exec.rs` calls it as `exec_instr::from`.
use lightr_core::{LightrError, Result};

use super::{default_shell, BuildCtx, ImageConfig};
use crate::build::parse::Instr;
use crate::build::vars::interpolate;

/// `FROM`: hydrate the base image into a cleared work dir and (re)seed the
/// interpolation scope from the base config ENV + the stage ARG boundary.
///
/// WP-C: when the FROM carries `--platform=<p>`, the requested platform is
/// `${VAR}`-interpolated (Docker allows `FROM --platform=$TARGETPLATFORM`) and
/// VALIDATED against the base image's actual (single-arch) platform BEFORE
/// hydration — a mismatch fails closed (lightr cannot select a different
/// platform from a single-arch import). Absent flag ⇒ host platform ⇒ no
/// validation ⇒ byte-identical to the pre-WP body.
pub(in crate::build) fn from(ctx: &mut BuildCtx, instr: &Instr, image_ref: &str) -> Result<()> {
    // FROM ref is interpolated against the GLOBAL ARG scope (Docker:
    // ARG-before-FROM is usable here); multi-stage refs are DF-03.
    let image_ref = interpolate(image_ref, ctx.scope, ctx.escape)?;
    // WP-C: interpolate the requested `--platform` (if any) and validate it
    // against the base image's recorded platform — honest error on mismatch,
    // never a silent ignore. `None` (no flag) is a trivial pass (host default).
    let requested_platform = match instr {
        Instr::From {
            platform: Some(p), ..
        } => Some(interpolate(p, ctx.scope, ctx.escape)?),
        _ => None,
    };
    crate::build::platform::validate_against_base(
        ctx.store,
        &image_ref,
        requested_platform.as_deref(),
    )?;
    for entry in std::fs::read_dir(ctx.work_dir).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let p = entry.path();
        if p.is_dir() && !p.is_symlink() {
            std::fs::remove_dir_all(&p).map_err(LightrError::Io)?;
        } else {
            std::fs::remove_file(&p).map_err(LightrError::Io)?;
        }
    }
    if image_ref != "scratch" {
        lightr_index::hydrate(ctx.work_dir, ctx.store, &image_ref)?;
    }
    // Seed the interpolation scope from the base image's config ENV.
    // The hydrated base carries lightr's `.lightr-image.json` sidecar
    // (env/cmd/labels) for lightr-built bases; absent (e.g. scratch
    // or an OCI base without the sidecar) → empty, per the design.
    let base = ImageConfig::load(ctx.work_dir);
    *ctx.accumulated_env = base.env.clone();
    ctx.scope.env = ctx.accumulated_env.iter().cloned().collect();
    // Stage boundary: global ARGs do NOT cross into the stage (Docker).
    ctx.arg_state
        .sync(instr, ctx.arg_overrides, &mut ctx.scope.args);
    // SHELL is per-stage (WP-DF-09): a new stage resets to the default shell.
    *ctx.current_shell = default_shell();
    Ok(())
}
