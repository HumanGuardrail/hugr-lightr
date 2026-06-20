//! Build memoization: ImageMeta sidecar, step key, COPY hashing, TempDirGuard.
//!
//! # R-KEY partition (parity-contract.md §0) — DOCUMENTED here; behaviour is the WPs'
//!
//! The freeze-gate only DOCUMENTS the BUILD-key partition; `step_key` below is
//! UNCHANGED (the WPs implement the new inputs). BUILD-domain key inputs the
//! campaign enforces:
//!
//! - **IN the build key:** prev-layer root, the instruction's canonical text,
//!   COPY source content digests, and (WP-DF-13) the POST-INTERPOLATION
//!   instruction text + workdir/user/entrypoint-when-set + image ENV.
//! - **OUT of the build key:** runtime-only knobs (caps, ports, labels at RUN
//!   time) — those live in the RUN domain (see lightr-run/src/run/memo.rs).
//!
//! ## Per-domain v2 rule (LEAD ARBITRATION)
//!
//! The domain tag is bumped PER-KEY-DOMAIN, ONLY when that key's input format
//! changes. The BUILD key STAYS `lightr/build/v1` until **WP-DF-13**, which
//! bumps it to `lightr/build/v2` when post-interpolation text +
//! workdir/user/entrypoint enter the key. The RUN key is independent and stays
//! `lightr/run/v1`. Each bump is a documented one-time Action-Cache
//! invalidation. The freeze-gate does NOT bump anything.
use lightr_core::{Digest, LightrError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::parse::{BuildStep, Instr};

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

/// Compute `step_key = BLAKE3("lightr/build/v1" | prev_root_bytes |
/// instr_canonical_bytes | [for COPY: each file's digest])`.
pub(crate) fn step_key(
    prev_layer_root: Option<Digest>,
    step: &BuildStep,
    context_dir: &Path,
) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lightr/build/v1");
    let prev_bytes = prev_layer_root.map(|d| d.0).unwrap_or([0u8; 32]);
    hasher.update(&prev_bytes);
    // canonical instr bytes = the raw line text
    hasher.update(step.raw.as_bytes());
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

    #[test]
    fn step_key_dir_copy_changes_when_contained_file_changes() {
        // `COPY src/ /app` must invalidate the cache when a file INSIDE src/
        // changes -- not just top-level files.
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

        let k1 = step_key(None, &step, ctx.path()).unwrap();

        // change a NESTED file
        std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-two").unwrap();
        let k2 = step_key(None, &step, ctx.path()).unwrap();
        assert_ne!(
            k1.0, k2.0,
            "nested file change must change the COPY step key"
        );

        // adding a file changes the key too
        std::fs::write(ctx.path().join("src/c.txt"), b"new").unwrap();
        let k3 = step_key(None, &step, ctx.path()).unwrap();
        assert_ne!(k2.0, k3.0, "adding a file must change the COPY step key");

        // identical content => identical key (determinism)
        std::fs::remove_file(ctx.path().join("src/c.txt")).unwrap();
        std::fs::write(ctx.path().join("src/nested/b.txt"), b"deep-one").unwrap();
        let k4 = step_key(None, &step, ctx.path()).unwrap();
        assert_eq!(k1.0, k4.0, "restoring content must restore the key");
    }
}
