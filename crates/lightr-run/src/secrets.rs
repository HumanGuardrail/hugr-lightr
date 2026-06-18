//! Store-backed secrets/configs for a run (F-309).
//!
//! build-spec-parity.md §A0.2 freezes these seams; **WP-A3 fills the bodies**.
//! A0 ships inert stubs:
//!   * `contribute_to_key` MUST stay a no-op so existing memo keys are
//!     UNCHANGED and every existing test stays green (it becomes the real
//!     in-key contribution only in WP-A3).
//!   * `hydrate` returns `Ok(())` (no files materialized yet).

use crate::StoreFile;
use lightr_core::Result;
use lightr_store::Store;
use std::path::Path;

/// Contribute the secrets/configs (in `files`, under `domain`) to the memo key.
///
/// A0 stub: **no-op** — deliberately leaves the hasher untouched so the key is
/// byte-identical to today's. WP-A3 hashes, per file sorted by name,
/// `name + \0 + resolved-manifest-digest` under the given domain.
pub fn contribute_to_key(hasher: &mut blake3::Hasher, files: &[StoreFile], domain: &[u8]) {
    // No-op in A0. Reference the params so the seam is explicit and clippy is
    // satisfied; touching `hasher` here would change the key (forbidden in A0).
    let _ = (hasher, files, domain);
}

/// Materialize secrets/configs into the run cwd (on a cache miss). A0 stub: no-op.
/// WP-A3 hydrates each ref via `lightr_index` into `<cwd>/.lightr/secrets/<name>`
/// (chmod 0600) / `<cwd>/.lightr/configs/<name>` (0644), failing closed on a
/// missing ref.
pub fn hydrate(
    cwd: &Path,
    store: &Store,
    secrets: &[StoreFile],
    configs: &[StoreFile],
) -> Result<()> {
    let _ = (cwd, store, secrets, configs);
    Ok(())
}
