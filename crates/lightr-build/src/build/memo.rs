//! Build memoization: ImageMeta sidecar, step key, COPY hashing, TempDirGuard.
//!
//! # R-KEY partition (parity-contract.md §0) — DOCUMENTED here; behaviour is the WPs'
//!
//! The freeze-gate only DOCUMENTS the BUILD-key partition; `step_key` below is
//! UNCHANGED (the WPs implement the new inputs). BUILD-domain key inputs the
//! campaign enforces:
//!
//! - **IN the build key:** prev-layer root, the instruction's **post-interpolation**
//!   canonical text (WP-DF-BUILDKEY), and COPY source content digests.
//!   (workdir/user/entrypoint-when-set + image ENV-as-distinct-input land with
//!   later DF WPs; today image ENV enters indirectly via the interpolated text.)
//! - **OUT of the build key:** runtime-only knobs (caps, ports, labels at RUN
//!   time) — those live in the RUN domain (see lightr-run/src/run/memo.rs).
//!
//! ## Per-domain v2 rule (LEAD ARBITRATION)
//!
//! The domain tag is bumped PER-KEY-DOMAIN, ONLY when that key's input format
//! changes. WP-DF-BUILDKEY bumps the BUILD key to `lightr/build/v2`: the key now
//! hashes the **post-interpolation** instruction text (Docker interpolates
//! `${VAR}` at build time, so two builds with different ENV/ARG but identical raw
//! text must NOT collide on a stale layer). The RUN key is independent and stays
//! `lightr/run/v1`. The v2 bump is a documented ONE-TIME Action-Cache
//! invalidation: every image rebuilds once (expected + acceptable). A Dockerfile
//! with no `${VAR}` re-keys once to v2 and is then stable across runs.
use lightr_core::{Digest, LightrError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::parse::{BuildStep, Instr};
use super::vars::{interpolate, VarScope};

/// Sidecar `.lightr-image.json` stored at the layer root.
/// Persists CMD / LABEL / ENV accumulation across layer snapshots.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ImageMeta {
    pub cmd: Option<Vec<String>>,
    pub labels: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
}

pub(crate) const IMAGE_META_FILE: &str = ".lightr-image.json";

pub(crate) fn load_meta(root: &Path) -> ImageMeta {
    let p = root.join(IMAGE_META_FILE);
    if let Ok(bytes) = std::fs::read(&p) {
        serde_json::from_slice(&bytes).unwrap_or_default()
    } else {
        ImageMeta::default()
    }
}

pub(crate) fn save_meta(root: &Path, meta: &ImageMeta) -> Result<()> {
    let bytes = serde_json::to_vec(meta)
        .map_err(|e| LightrError::InvalidManifest(format!("meta serialize: {e}")))?;
    std::fs::write(root.join(IMAGE_META_FILE), &bytes).map_err(LightrError::Io)
}

/// The BUILD-key domain tag. **v2** (WP-DF-BUILDKEY): the key hashes the
/// POST-INTERPOLATION instruction text, not the raw text. Bumping from v1 is a
/// documented one-time Action-Cache invalidation (every image rebuilds once).
pub(crate) const BUILD_KEY_DOMAIN: &[u8] = b"lightr/build/v2";

/// Canonical instruction text for keying: `step.raw` after `${VAR}`
/// interpolation against `scope` (honoring the Dockerfile escape directive).
///
/// This is the single hashed representation of the instruction's text — it
/// captures the interpolation of EVERY text arg in one pass, so two builds whose
/// ENV/ARG resolve `${VAR}` differently produce different canonical text and
/// therefore different keys (no false memo hit). A no-`${VAR}` line interpolates
/// to itself verbatim, so the key is behavior-preserving modulo the v1→v2 bump.
pub(crate) fn canonical_step_text(
    step: &BuildStep,
    scope: &VarScope,
    escape: bool,
) -> Result<String> {
    interpolate(&step.raw, scope, escape)
}

/// Compute `step_key = BLAKE3(BUILD_KEY_DOMAIN | prev_root_bytes |
/// post_interpolation_text | [for COPY: each file's digest])`.
///
/// `scope` + `escape` interpolate the instruction text BEFORE hashing (Docker
/// resolves `${VAR}` at build time; the cache key must reflect the resolved
/// text, never the raw text — else differing ENV/ARG collide on a stale layer).
pub(crate) fn step_key(
    prev_layer_root: Option<Digest>,
    step: &BuildStep,
    context_dir: &Path,
    scope: &VarScope,
    escape: bool,
) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(BUILD_KEY_DOMAIN);
    let prev_bytes = prev_layer_root.map(|d| d.0).unwrap_or([0u8; 32]);
    hasher.update(&prev_bytes);
    // canonical instr bytes = the POST-INTERPOLATION line text
    let text = canonical_step_text(step, scope, escape)?;
    hasher.update(text.as_bytes());
    // For COPY, hash each source's content into the key. Files contribute
    // their digest; DIRECTORIES contribute every contained file's
    // (relative-path | digest), sorted -- so editing any file inside a copied
    // dir (e.g. `COPY src/ /app`) invalidates the cache. Symlinks contribute
    // their target. Missing sources contribute a sentinel (so add/remove of a
    // source also changes the key).
    if let Instr::Copy { src, .. } = &step.instr {
        for s in src {
            let src_path = context_dir.join(s);
            hash_copy_source(&mut hasher, &src_path)?;
        }
    }
    Ok(Digest(*hasher.finalize().as_bytes()))
}

/// Fold a COPY source's content-identity into `hasher`, recursing dirs.
pub(crate) fn hash_copy_source(hasher: &mut blake3::Hasher, path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => {
            hasher.update(b"\x00missing\x00");
            return Ok(());
        }
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = std::fs::read_link(path).map_err(LightrError::Io)?;
        hasher.update(b"L");
        hasher.update(target.as_os_str().as_encoded_bytes());
    } else if ft.is_file() {
        hasher.update(b"F");
        hasher.update(&Digest::of_file(path)?.0);
    } else if ft.is_dir() {
        hasher.update(b"D");
        // Collect (relative path, entry) deterministically (sorted by path).
        let mut entries: Vec<PathBuf> = Vec::new();
        collect_dir_paths(path, &mut entries)?;
        entries.sort();
        for child in &entries {
            let rel = child.strip_prefix(path).unwrap_or(child);
            hasher.update(rel.as_os_str().as_encoded_bytes());
            hasher.update(b"\x00");
            hash_copy_source(hasher, child)?;
        }
    }
    Ok(())
}

/// Recursively collect every entry path under `dir` (files, dirs, symlinks).
pub(crate) fn collect_dir_paths(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(LightrError::Io)? {
        let entry = entry.map_err(LightrError::Io)?;
        let p = entry.path();
        let ft = entry.file_type().map_err(LightrError::Io)?;
        out.push(p.clone());
        if ft.is_dir() {
            collect_dir_paths(&p, out)?;
        }
    }
    Ok(())
}

pub(crate) struct TempDirGuard {
    pub path: PathBuf,
}

impl TempDirGuard {
    pub fn new() -> Result<Self> {
        let base = std::env::temp_dir();
        let unique = format!(
            "lightr-build-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = base.join(unique);
        std::fs::create_dir_all(&path).map_err(LightrError::Io)?;
        Ok(TempDirGuard { path })
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // A scope from (arg, env) pairs, for keying tests.
    fn scope(args: &[(&str, &str)], envs: &[(&str, &str)]) -> VarScope {
        VarScope {
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            env: envs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn run_step(raw: &str) -> BuildStep {
        // The instr variant is irrelevant for keying (the key hashes the
        // interpolated raw text + COPY content); a RUN step keeps the test
        // self-contained.
        BuildStep {
            instr: Instr::Run {
                argv: vec!["/bin/sh".into(), "-c".into(), raw.into()],
                form: super::super::parse::CmdForm::Shell(raw.into()),
            },
            raw: raw.to_string(),
        }
    }

    #[test]
    fn step_key_dir_copy_changes_when_contained_file_changes() {
        // `COPY src/ /app` must invalidate the cache when a file INSIDE src/
        // changes -- not just top-level files.
        // NOTE: step_key now takes (scope, escape) — WP-DF-BUILDKEY. An empty
        // scope leaves the COPY text verbatim, so this content-fingerprint
        // assertion is unchanged in meaning (only the v2 tag changed the bytes).
        let ctx = TempDir::new().unwrap();
        std::fs::create_dir_all(ctx.path().join("src/nested")).unwrap();
        std::fs::write(ctx.path().join("src/a.txt"), b"one").unwrap();
        std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-one").unwrap();

        let step = BuildStep {
            instr: Instr::Copy {
                src: vec!["src".to_string()],
                dest: "/app".to_string(),
                from: None,
                chown: None,
                chmod: None,
            },
            raw: "COPY src /app".to_string(),
        };
        let s = VarScope::default();

        let k1 = step_key(None, &step, ctx.path(), &s, true).unwrap();

        // change a NESTED file
        std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-two").unwrap();
        let k2 = step_key(None, &step, ctx.path(), &s, true).unwrap();
        assert_ne!(
            k1.0, k2.0,
            "nested file change must change the COPY step key"
        );

        // adding a file changes the key too
        std::fs::write(ctx.path().join("src/c.txt"), b"new").unwrap();
        let k3 = step_key(None, &step, ctx.path(), &s, true).unwrap();
        assert_ne!(k2.0, k3.0, "adding a file must change the COPY step key");

        // identical content => identical key (determinism)
        std::fs::remove_file(ctx.path().join("src/c.txt")).unwrap();
        std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-one").unwrap();
        let k4 = step_key(None, &step, ctx.path(), &s, true).unwrap();
        assert_eq!(k1.0, k4.0, "restoring content must restore the key");
    }

    // ---- WP-DF-BUILDKEY: MEMO-CORRECTNESS at the key layer ----

    #[test]
    fn interp_var_value_change_changes_key_no_false_hit() {
        // The SAME raw instruction `RUN echo ${X}` keyed under X=A vs X=B must
        // produce DIFFERENT keys — else B would reuse A's cached layer (silent
        // WRONG build). This is the core memoization-correctness invariant.
        let ctx = TempDir::new().unwrap();
        let step = run_step("RUN echo ${X}");

        let sa = scope(&[], &[("X", "alpha")]);
        let sb = scope(&[], &[("X", "beta")]);

        let ka = step_key(None, &step, ctx.path(), &sa, true).unwrap();
        let kb = step_key(None, &step, ctx.path(), &sb, true).unwrap();
        assert_ne!(
            ka.0, kb.0,
            "differing ${{X}} values must yield differing keys (no false memo hit)"
        );
    }

    #[test]
    fn interp_same_inputs_same_key_memo_hit() {
        // Identical (instruction, scope) ⇒ identical key ⇒ memo HIT.
        let ctx = TempDir::new().unwrap();
        let step = run_step("RUN echo ${X}-${Y}");
        let s = scope(&[("Y", "two")], &[("X", "one")]);

        let k1 = step_key(None, &step, ctx.path(), &s, true).unwrap();
        let k2 = step_key(None, &step, ctx.path(), &s, true).unwrap();
        assert_eq!(k1.0, k2.0, "identical inputs must yield an identical key");
    }

    #[test]
    fn no_var_dockerfile_key_is_stable() {
        // A line with no `${VAR}` keys identically regardless of scope, and is
        // stable across runs (v2). Behavior-preserving modulo the v1→v2 bump.
        let ctx = TempDir::new().unwrap();
        let step = run_step("RUN echo hello");
        let empty = VarScope::default();
        let populated = scope(&[("X", "v")], &[("Y", "w")]);

        let k1 = step_key(None, &step, ctx.path(), &empty, true).unwrap();
        let k2 = step_key(None, &step, ctx.path(), &empty, true).unwrap();
        let k3 = step_key(None, &step, ctx.path(), &populated, true).unwrap();
        assert_eq!(k1.0, k2.0, "no-var key must be stable across runs");
        assert_eq!(k1.0, k3.0, "no-var key must not depend on scope contents");
    }

    #[test]
    fn v2_domain_tag_in_key() {
        // Document + lock the one-time invalidation: the domain tag is v2.
        assert_eq!(BUILD_KEY_DOMAIN, b"lightr/build/v2");
    }
}
