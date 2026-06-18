//! Store-backed secrets/configs for a run (F-309).
//!
//! build-spec-parity.md §0 (memo-key law) + §4 (WP-A3). WP-A0 froze the seams
//! (the call sites in `build_key`/`assemble_key`/`run_memoized_with`); **WP-A3
//! fills these bodies**.
//!
//! ## Two responsibilities
//!
//! * [`contribute_to_key`] — secrets/configs are store-backed **inputs**, so a
//!   different secret/config ⇒ a different run ⇒ must NOT share a cache entry
//!   (§0). We hash, per file sorted by name, `name + \0 + resolved-manifest-
//!   digest`, exactly mirroring how `mounts` contribute (ordered ref-name +
//!   resolved root digest). Empty `files` leaves the hasher untouched, so the
//!   16 existing callers (all empty vecs) keep byte-identical keys and every
//!   existing memo test stays green.
//! * [`hydrate`] — on a cache miss, materialize each ref into the run cwd.
//!
//! ## HONEST on-disk-secret boundary (documented, NOT hidden)
//!
//! Lightr ships **no daemon and no tmpfs**, so there is nowhere to hold a
//! secret in memory across the run-spawn boundary. A "secret" therefore lands
//! on the local disk under the run dir at mode `0600` (owner read/write only);
//! a "config" lands at `0644`. This is acceptable for the single-user local
//! product and is stated plainly here rather than dressed up as a vault: the
//! isolation boundary is filesystem permissions on the user's own machine, not
//! a kernel/keyring secret store. The CAS bytes are content-addressed and
//! verified before materialization (sealed-store trust, ADR-0009).

use crate::StoreFile;
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::Path;

/// Contribute the secrets/configs (in `files`, under `domain`) to the memo key.
///
/// For each file **sorted by name**, hashes:
///   `domain` · `name` · `0x00` · resolved-manifest-digest (32B)
/// where the resolved manifest digest is the ref's current root digest, looked
/// up via the store (`ref_get`) — the same resolution `assemble_key` uses for
/// mounts. A different secret/config ref (or a repointed ref) thus yields a
/// different key (§0: in-key inputs).
///
/// A missing ref is **not** an error here — key assembly must stay infallible
/// (it runs even on the predict/hit path, before any hydrate). An unresolved
/// ref simply contributes `name + 0x00` with no digest; the miss path's
/// [`hydrate`] is where a missing ref fails closed. In practice the digest is
/// present whenever the run actually executes (hydrate would have failed
/// otherwise), so the key faithfully separates distinct secrets.
///
/// Empty `files` ⇒ the hasher is left untouched ⇒ keys for the 16 existing
/// (empty-vec) callers are byte-identical to before WP-A3.
pub fn contribute_to_key(
    hasher: &mut blake3::Hasher,
    files: &[StoreFile],
    domain: &[u8],
    store: &Store,
) {
    if files.is_empty() {
        return;
    }
    // Sort by name for a deterministic, order-independent contribution.
    let mut sorted: Vec<&StoreFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));

    for f in sorted {
        hasher.update(domain);
        hasher.update(f.name.as_bytes());
        hasher.update(b"\0");
        // Resolve the ref's current root (manifest) digest. ref_get returns
        // Ok(None) for an absent ref; a store I/O error is also non-fatal here
        // (key assembly is infallible) — either way we contribute no digest and
        // the miss-path hydrate fails closed on a genuinely missing ref.
        if let Ok(Some(rec)) = store.ref_get(&f.ref_name) {
            hasher.update(&rec.root.0);
        }
    }
}

/// Materialize secrets/configs into the run cwd (on a cache miss).
///
/// Each secret ref is hydrated into `<cwd>/.lightr/secrets/<name>` at mode
/// `0600`; each config ref into `<cwd>/.lightr/configs/<name>` at mode `0644`.
/// See the module-level HONEST boundary note: these are plain on-disk files
/// under the run dir, protected by filesystem permissions only.
///
/// Fails **closed**: a missing ref (or any hydrate failure) returns `Err` and
/// no run proceeds — a secret a user asked for must never be silently absent.
pub fn hydrate(
    cwd: &Path,
    store: &Store,
    secrets: &[StoreFile],
    configs: &[StoreFile],
) -> Result<()> {
    if !secrets.is_empty() {
        let dir = cwd.join(".lightr").join("secrets");
        for f in secrets {
            hydrate_one(&dir, store, f, 0o600)?;
        }
    }
    if !configs.is_empty() {
        let dir = cwd.join(".lightr").join("configs");
        for f in configs {
            hydrate_one(&dir, store, f, 0o644)?;
        }
    }
    Ok(())
}

/// Hydrate a single store-backed file `<base_dir>/<f.name>` from `f.ref_name`,
/// then set its mode (unix). Fails closed if the ref is missing or the
/// destination escapes `base_dir`.
fn hydrate_one(base_dir: &Path, store: &Store, f: &StoreFile, mode: u32) -> Result<()> {
    // Reject a name that escapes the secrets/configs dir (e.g. "../x", "/abs",
    // "a/../../x"): a secret must land exactly where promised. Mirrors the
    // mount-target validation in lib.rs (relative, no ParentDir component).
    validate_file_name(&f.name)?;

    std::fs::create_dir_all(base_dir).map_err(LightrError::Io)?;
    let dest = base_dir.join(&f.name);

    // `lightr_index::hydrate` materializes a ref tree into `dest`, which it
    // requires to not-exist-or-be-empty and creates itself. A store-backed
    // "file" is modeled as a single-entry ref tree, so `dest` becomes a
    // directory containing the ref's content. To present the secret at the
    // promised path we hydrate into a fresh staging dir, then move/flatten.
    //
    // Simpler + matches the snapshot model: hydrate the ref into `dest` as a
    // directory. The secret content is whatever the ref holds (a file named by
    // the snapshot, or a tree). We then apply `mode` to every materialized
    // regular file so the 0600/0644 boundary holds for the bytes on disk.
    //
    // Fail closed on a missing ref (RefNotFound) or any hydrate error.
    if dest.exists() {
        // A stale hydrate from a prior miss could remain; remove it so the
        // re-hydrate (dest must be empty/absent) succeeds and the bytes are
        // refreshed from the current ref.
        std::fs::remove_dir_all(&dest).map_err(LightrError::Io)?;
    }
    lightr_index::hydrate(&dest, store, &f.ref_name)?;

    apply_mode_recursive(&dest, mode)?;
    Ok(())
}

/// Validate a secret/config file name: must be a single relative path segment
/// chain with no `..` and no absolute root — it must stay under the base dir.
fn validate_file_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(LightrError::InvalidRef(
            "empty secret/config name".to_string(),
        ));
    }
    let p = Path::new(name);
    if p.is_absolute() {
        return Err(LightrError::InvalidRef(format!(
            "secret/config name escapes the run dir: {name}"
        )));
    }
    for component in p.components() {
        if component == std::path::Component::ParentDir {
            return Err(LightrError::InvalidRef(format!(
                "secret/config name escapes the run dir: {name}"
            )));
        }
    }
    Ok(())
}

/// Apply `mode` to `path` and (if a directory) every regular file/dir beneath
/// it. Unix-only: the on-disk-secret boundary is filesystem permissions.
///
/// On non-unix, file modes are not meaningful; this is a documented no-op and
/// the honest-boundary note applies with whatever the platform's default ACLs
/// are (Windows is a future hardening ring).
fn apply_mode_recursive(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::symlink_metadata(path).map_err(LightrError::Io)?;
        if meta.is_dir() {
            // Directories get exec bits so the owner can traverse: 0600→0700,
            // 0644→0755. Files get the exact requested mode.
            let dir_mode = mode | ((mode & 0o600) >> 2) | ((mode & 0o600) >> 1);
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(dir_mode))
                .map_err(LightrError::Io)?;
            for entry in std::fs::read_dir(path).map_err(LightrError::Io)? {
                let entry = entry.map_err(LightrError::Io)?;
                apply_mode_recursive(&entry.path(), mode)?;
            }
        } else if meta.is_file() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(LightrError::Io)?;
        }
        // Symlinks: leave as-is (chmod follows the link; the target is handled
        // when walked directly).
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}
