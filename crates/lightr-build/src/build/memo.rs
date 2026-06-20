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

use super::parse::{BuildStep, CmdForm, Instr};
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
/// post_interpolation_text | [for shell-form RUN: the active SHELL] |
/// [for COPY: each file's digest, then --chown/--chmod when present])`.
///
/// `scope` + `escape` interpolate the instruction text BEFORE hashing (Docker
/// resolves `${VAR}` at build time; the cache key must reflect the resolved
/// text, never the raw text — else differing ENV/ARG collide on a stale layer).
///
/// `current_shell` (WP-DF-09) is the active SHELL. It is NOT part of a RUN's
/// instruction text, yet it changes HOW a shell-form RUN executes — so two
/// builds with a different SHELL but identical `RUN cmd` text would otherwise
/// collide to the same key (a FALSE memo hit). The active shell is folded into
/// the key for **shell-form RUN only** (exec-form RUN ignores SHELL, matching
/// Docker — so it is NOT folded there, avoiding needless cache busts). Non-RUN
/// instructions never fold it, so their keys are byte-identical to before.
pub(crate) fn step_key(
    prev_layer_root: Option<Digest>,
    step: &BuildStep,
    context_dir: &Path,
    scope: &VarScope,
    escape: bool,
    current_shell: &[String],
) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(BUILD_KEY_DOMAIN);
    let prev_bytes = prev_layer_root.map(|d| d.0).unwrap_or([0u8; 32]);
    hasher.update(&prev_bytes);
    // canonical instr bytes = the POST-INTERPOLATION line text
    let text = canonical_step_text(step, scope, escape)?;
    hasher.update(text.as_bytes());
    // WP-DF-09: a SHELL-form RUN's key MUST depend on the active SHELL (it is
    // the actual interpreter, but not part of the RUN text). Folded under a
    // domain separator with NUL-delimited tokens, so different SHELL ⇒ different
    // key ⇒ no false cache hit. Exec-form RUN (and all other instructions) do
    // NOT fold it, so their keys are unchanged from before this WP.
    if let Instr::Run {
        form: CmdForm::Shell(_),
        ..
    } = &step.instr
    {
        hasher.update(b"\x00shell\x00");
        for tok in current_shell {
            hasher.update(tok.as_bytes());
            hasher.update(b"\x00");
        }
    }
    // For COPY and ADD, hash each source's content into the key. Files contribute
    // their digest; DIRECTORIES contribute every contained file's
    // (relative-path | digest), sorted -- so editing any file inside a copied
    // dir (e.g. `COPY src/ /app`) invalidates the cache. Symlinks contribute
    // their target. Missing sources contribute a sentinel (so add/remove of a
    // source also changes the key).
    //
    // WP-DF-07: ADD keys IDENTICALLY to COPY — it hashes the same source content
    // and folds the same --chown/--chmod. ADD's auto-extraction is a DETERMINISTIC
    // function of the keyed archive bytes (the .tar's content digest already enters
    // via `hash_copy_source`), so no extra extraction input is needed: same archive
    // + same flags ⇒ same key ⇒ same extracted layer. The two variants share this
    // block by destructuring the common `(src, chown, chmod)` fields.
    if let Instr::Copy {
        src, chown, chmod, ..
    }
    | Instr::Add {
        src, chown, chmod, ..
    } = &step.instr
    {
        for s in src {
            let src_path = context_dir.join(s);
            hash_copy_source(&mut hasher, &src_path)?;
        }
        // WP-DF-06: --chown/--chmod change the COPY/ADD OUTPUT (file mode/owner)
        // but are NOT part of the source content the loop above hashes. Two of
        // the same bytes with different --chmod/--chown produce different layers,
        // so the flags MUST enter the key — else the second would FALSELY hit the
        // first's cached layer (wrong mode/owner).
        //
        // The flags' POST-INTERPOLATION text already enters the key via
        // `canonical_step_text` (the flags live in `step.raw`). This explicit
        // fold is the LOCAL, refactor-proof guarantee the contract requires: it
        // pins the no-false-hit invariant to THIS block rather than to how the
        // raw line happens to be composed. Folded with the SAME interpolated
        // value that `exec_instr::copy` applies, ONLY when the flag is present,
        // each under its own NUL-delimited separator — so a flagless
        // `COPY src dest` keys BYTE-IDENTICALLY to before this WP (no cache bust).
        if let Some(c) = chown {
            hasher.update(b"\x00chown\x00");
            hasher.update(interpolate(c, scope, escape)?.as_bytes());
        }
        if let Some(c) = chmod {
            hasher.update(b"\x00chmod\x00");
            hasher.update(interpolate(c, scope, escape)?.as_bytes());
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

// Tests live in a sibling file (`#[path]`) to keep this file under the 400-line
// godfile cap after the WP-DF-09 SHELL-key additions (house convention).
#[cfg(test)]
#[path = "memo_tests.rs"]
mod tests;
