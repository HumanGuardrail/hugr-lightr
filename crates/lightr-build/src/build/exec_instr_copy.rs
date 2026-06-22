//! SKELETON-FREEZE: `COPY`/`ADD` instruction bodies (they share the
//! glob/resolve/placement infra), split from `exec_instr.rs` so a WP touching
//! file-placement edits only this file. Behavior-preserving (byte-identical
//! logic to the prior single `exec_instr.rs`); re-exported from `exec_instr` so
//! `exec.rs` calls them as `exec_instr::{copy,add}`.
use lightr_core::{LightrError, Result};
use std::path::Path;

use super::{interp_vec, BuildCtx};
use crate::build::exec_fs::{expand_glob, materialize_from_digest, place_sources, CopyMeta};
use crate::build::memo::TempDirGuard;
use crate::build::vars::interpolate;

// WP-G: ADD `<url> <dest>` remote fetch lives in a sibling file, declared as a
// `#[path]` submodule here (mirrors how `exec_fs` hosts `exec_fs_tar`). Keeps the
// network concern + its tests isolated and this file's COPY/ADD logic lean.
#[path = "exec_add_url.rs"]
mod add_url;

/// `COPY [--from=<stage>] [--chown=u:g] [--chmod=NNNN] <src>... <dest>`
/// (WP-DF-06 + WP-DF-03 multi-stage).
///
/// `--from=<stage-name|stage-index>` (WP-DF-03) copies from a PRIOR stage's
/// RESULTING filesystem instead of the build context: the stage's output tree is
/// materialized from the CAS into a temp dir, and the sources are resolved
/// against THAT dir. Unknown / self / forward stage refs are an honest error
/// (resolved by [`StageTable::resolve`]); an external IMAGE `--from` is OUT OF
/// SCOPE for this WP (also surfaced by `resolve` as "unknown stage / external
/// image out of scope"). Without `--from`, behavior is byte-identical to before:
/// sources resolve against the build context.
///
/// `--chown`/`--chmod` (parsed into [`CopyMeta`]), multi-src/glob/dir-contents
/// placement, and the dir-vs-file dest rule all live in `place_sources`. The memo
/// key folds chown/chmod + the resolved source content AND (for `--from`) the
/// upstream stage's output digest (build/memo.rs), so this executor only realizes
/// the bytes + metadata.
///
/// [`StageTable::resolve`]: crate::build::exec::StageTable::resolve
pub(in crate::build) fn copy(
    ctx: &mut BuildCtx,
    src: &[String],
    dest: &str,
    from: Option<&str>,
    chown: Option<&str>,
    chmod: Option<&str>,
) -> Result<()> {
    let meta = CopyMeta::parse(chown, chmod)?;
    let dest = &interpolate(dest, ctx.scope, ctx.escape)?;
    if let Some(from_ref) = from {
        // WP-DF-03: COPY --from=<stage>. Resolve the prior stage's output tree,
        // materialize it into an isolated temp dir, and copy from THERE (not the
        // build context). The temp dir is dropped after placement (TempDirGuard).
        let from_ref = interpolate(from_ref, ctx.scope, ctx.escape)?;
        let digest = ctx.stages.resolve(&from_ref)?;
        let stage_guard = TempDirGuard::new()?;
        materialize_from_digest(&stage_guard.path, &digest, ctx.store)?;
        // Stage sources are filesystem paths (typically ABSOLUTE, e.g.
        // `/out/app`). They are resolved relative to the materialized stage TREE,
        // so a leading `/` is stripped before joining (a `Path::join` of an
        // absolute path would otherwise discard the stage root). `relative = true`.
        let sources = resolve_sources_in(&stage_guard.path, ctx, src, "COPY --from", true)?;
        // `--from` copies a PRIOR stage's tree, NOT the build context, so
        // `.dockerignore` does NOT apply — pass `None` (no filter).
        return place_sources(ctx.work_dir, &sources, dest, &meta, false, None);
    }
    let sources = resolve_sources(ctx, src, "COPY")?;
    // WP-DF-IGNORE: a nested file under a copied DIR (e.g. `COPY . /dst`) that the
    // matcher excludes is dropped during recursion. `Some((context_dir, ignore))`
    // only when the matcher has rules — else `None` keeps the copy byte-identical.
    let filter = ctx_filter(ctx);
    // COPY never auto-extracts (a `.tar` is copied as a file) — `extract = false`.
    place_sources(ctx.work_dir, &sources, dest, &meta, false, filter)
}

/// The `.dockerignore` filter pair `(context_root, matcher)` for context COPY/ADD
/// — `None` when there are no rules (so the copy path is byte-identical to before
/// this WP). `COPY --from=stage` never uses it (a stage tree is not the context).
fn ctx_filter<'a>(
    ctx: &'a BuildCtx,
) -> Option<(&'a Path, &'a crate::build::dockerignore::DockerIgnore)> {
    (!ctx.ignore.is_inactive()).then_some((ctx.context_dir, ctx.ignore))
}

/// Interpolate + glob-expand a COPY/ADD `src` list against the build context. A
/// glob with zero matches is an honest error (Docker: "no source files"); a
/// literal token is kept verbatim. Shared by COPY+ADD (DF-07 reuses DF-06).
fn resolve_sources(ctx: &BuildCtx, src: &[String], verb: &str) -> Result<Vec<std::path::PathBuf>> {
    // Context sources are context-RELATIVE; `relative = false` preserves the
    // exact pre-WP join behavior (`context_dir.join(token)` verbatim).
    resolve_sources_in(ctx.context_dir, ctx, src, verb, false)
}

/// Like [`resolve_sources`] but resolves against an arbitrary `root` (WP-DF-03:
/// the build context for a plain COPY, or a materialized PRIOR-stage tree for
/// `COPY --from=stage`). Identical glob/honest-error semantics. When `relative`,
/// a leading `/` is stripped from each (interpolated) token so an ABSOLUTE stage
/// path resolves UNDER `root` instead of escaping it via `Path::join`.
fn resolve_sources_in(
    root: &Path,
    ctx: &BuildCtx,
    src: &[String],
    verb: &str,
    relative: bool,
) -> Result<Vec<std::path::PathBuf>> {
    let raw_src = interp_vec(src, ctx.scope, ctx.escape)?;
    let mut sources: Vec<std::path::PathBuf> = Vec::new();
    for token in &raw_src {
        let token = if relative {
            token.trim_start_matches('/')
        } else {
            token.as_str()
        };
        let matched = expand_glob(root, token);
        if (token.contains('*') || token.contains('?')) && matched.is_empty() {
            return Err(LightrError::InvalidManifest(format!(
                "{verb}: no source files match {token:?}"
            )));
        }
        // WP-DF-IGNORE: for a CONTEXT source (`!relative`), drop any TOP-LEVEL
        // glob match the matcher excludes (e.g. `COPY *.log` after `*.log` in
        // `.dockerignore` ⇒ zero sources). Nested files under a copied DIR are
        // filtered later, during recursive placement. `--from` (`relative`) is a
        // prior-stage tree, not the context, so it is never filtered here.
        if !relative && !ctx.ignore.is_inactive() {
            for m in matched {
                let keep = match m.strip_prefix(root) {
                    Ok(rel) => {
                        let rel = rel.to_string_lossy();
                        rel.is_empty() || !ctx.ignore.is_excluded(&rel)
                    }
                    Err(_) => true,
                };
                if keep {
                    sources.push(m);
                }
            }
        } else {
            sources.extend(matched);
        }
    }
    Ok(sources)
}

/// `ADD [--chown=u:g] [--chmod=NNNN] <src>... <dest>` (WP-DF-07 + WP-G).
///
/// Two source flavors, per Docker:
///   * a LOCAL src that is a recognized archive FILE (`.tar`, `.tar.gz`/`.tgz`,
///     `.tar.bz2`/`.tbz2`, `.tar.xz`/`.txz`) is auto-EXTRACTED into dest (all four
///     compressions now decode in-process — see `exec_fs_tar`); other local
///     file/dir sources behave exactly like COPY (`CopyMeta`/`place_sources`);
///   * a remote `http(s)://` URL is DOWNLOADED to dest and NEVER auto-extracted
///     (Docker treats a URL `.tar.gz` as an opaque file) — fetched into a temp
///     dir and placed with `extract = false` (WP-G, `add_url`).
///
/// Determinism (memo key): the key folds the instruction's canonical text (which
/// carries the URL) + chown/chmod, exactly as COPY folds context content. A URL
/// token is not a context path, so the content-hash loop folds a stable sentinel
/// — Docker likewise keys a URL ADD by the URL STRING, not remote bytes. So the
/// memo key stays intact: an unchanged ADD step is never re-fetched.
pub(in crate::build) fn add(
    ctx: &mut BuildCtx,
    src: &[String],
    dest: &str,
    chown: Option<&str>,
    chmod: Option<&str>,
) -> Result<()> {
    let meta = CopyMeta::parse(chown, chmod)?;
    let dest = &interpolate(dest, ctx.scope, ctx.escape)?;
    let filter = ctx_filter(ctx);

    // Partition tokens: remote URLs are fetched (no extract); the rest resolve
    // against the build context and auto-extract recognized archives. A URL token
    // is interpolated first so an ARG-built URL is honored.
    let mut url_tokens: Vec<String> = Vec::new();
    let mut local_tokens: Vec<String> = Vec::new();
    for token in src {
        let t = interpolate(token, ctx.scope, ctx.escape)?;
        if add_url::is_remote(&t) {
            url_tokens.push(t);
        } else {
            // Push the RAW token (not the interpolated form) so context globbing
            // + `.dockerignore` keep their byte-identical pre-WP behavior.
            local_tokens.push(token.clone());
        }
    }

    // Remote URLs: download each into a temp dir, then place with `extract=false`
    // (Docker never auto-extracts a URL). The guard drops the temp bytes after
    // placement, so a fetched archive never lingers outside the work dir.
    if !url_tokens.is_empty() {
        let dl_guard = TempDirGuard::new()?;
        let mut fetched: Vec<std::path::PathBuf> = Vec::with_capacity(url_tokens.len());
        for url in &url_tokens {
            fetched.push(add_url::fetch_into(url, &dl_guard.path)?);
        }
        // No `.dockerignore` for downloaded bytes (they are not context); `None`.
        place_sources(ctx.work_dir, &fetched, dest, &meta, false, None)?;
    }

    // Local sources: ADD auto-extracts recognized archives (`extract = true`);
    // placement + dir/file rules are COPY's, shared via `place_sources`. The same
    // `.dockerignore` filter as COPY applies to context sources (WP-DF-IGNORE).
    if !local_tokens.is_empty() {
        let sources = resolve_sources(ctx, &local_tokens, "ADD")?;
        place_sources(ctx.work_dir, &sources, dest, &meta, true, filter)?;
    }
    Ok(())
}
