//! lightr-store — frozen contract: build-spec v2 §4 (ADR-0009).
//! Object plane + refs + AC + CoW ladder. Bodies are WP-2.

use lightr_core::{Digest, RefRecord, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub mod store;

// Re-export the public surface that was previously flat in lib.rs.
pub use store::cow::CowRung;
pub use store::imgmeta::{ImageDescriptor, ImageManifestRecord};
pub use store::lock::{GcGuard, WriteGuard};
pub use store::volume::{self, VolumeInfo, DRIVER_LOCAL};

/// The lightr content-addressed store.
pub struct Store {
    pub(crate) root: PathBuf,
    rung: CowRung,
}

impl Store {
    /// Open (or create) a store at `root`.
    /// Creates objects/, refs/, ac/ top dirs lazily (shards created on write).
    /// Probes CoW rung.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root: PathBuf = root.into();
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("refs"))?;
        fs::create_dir_all(root.join("ac"))?;

        let rung = store::cow::probe_rung(&root);

        Ok(Store { root, rung })
    }

    /// Default root: $LIGHTR_HOME/store  (LIGHTR_HOME defaults to ~/.lightr).
    pub fn default_root() -> PathBuf {
        if let Some(home) = std::env::var_os("LIGHTR_HOME") {
            PathBuf::from(home).join("store")
        } else {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".lightr").join("store")
        }
    }

    /// Return the probed CoW rung.
    pub fn rung(&self) -> CowRung {
        self.rung
    }

    /// Store root path (gc walks objects from here). R1.
    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── gc lock ──────────────────────────────────────────────────────────────

    /// Acquire a SHARED advisory lock on `<root>/.gc.lock`.
    ///
    /// Writers (put_bytes, ingest_file, ref_put, ac_put) hold this for the
    /// duration of their write.  Multiple writers may proceed concurrently.
    /// gc's exclusive lock cannot be granted while any shared lock is held,
    /// so gc cannot sweep an object that a concurrent writer is mid-publishing.
    pub fn write_guard(&self) -> Result<WriteGuard> {
        store::lock::write_guard(&self.root)
    }

    /// Acquire an EXCLUSIVE advisory lock on `<root>/.gc.lock`.
    ///
    /// Held by gc for the full mark+sweep pass.  Blocks until all in-flight
    /// writer shared locks have been released.
    pub fn gc_guard(&self) -> Result<GcGuard> {
        store::lock::gc_guard(&self.root)
    }

    // ── CAS ──────────────────────────────────────────────────────────────────

    /// Content-address `bytes` and store them.  Idempotent: if the object
    /// already exists the digest is returned immediately without any write.
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        store::cas::put_bytes(&self.root, bytes)
    }

    /// Hash `path` and CoW-clone it into the store.  Idempotent.
    pub fn ingest_file(&self, path: &Path) -> Result<Digest> {
        store::cas::ingest_file(&self.root, path, self.rung)
    }

    /// Read and verify `d`.  Missing → NotFound.  Hash mismatch → Integrity
    /// (evidence file kept, never deleted).
    pub fn get_bytes(&self, d: &Digest) -> Result<Vec<u8>> {
        store::cas::get_bytes(&self.root, d)
    }

    /// Returns true iff the object file exists (no rehash).
    pub fn exists(&self, d: &Digest) -> bool {
        store::cas::exists(&self.root, d)
    }

    /// CoW the object identified by `d` to `dest`, then set its mode to
    /// `mode`.  Missing object → NotFound.  Parent dirs created if absent.
    pub fn materialize_file(&self, d: &Digest, dest: &Path, mode: u32) -> Result<()> {
        store::cas::materialize_file(&self.root, d, dest, mode, self.rung)
    }

    /// gc sweep only: chmod 0o644 then remove one object.
    /// Object absent ⇒ Ok(()) (idempotent).
    pub fn remove_object(&self, d: &Digest) -> Result<()> {
        store::cas::remove_object(&self.root, d)
    }

    // ── refs ─────────────────────────────────────────────────────────────────

    /// Read a ref.  `name` is validated; absent → Ok(None).
    pub fn ref_get(&self, name: &str) -> Result<Option<RefRecord>> {
        store::refs::ref_get(&self.root, name)
    }

    /// Write a ref atomically (last-write-wins).
    /// R1 extension: also writes a name record (once) and appends a log entry.
    pub fn ref_put(&self, rec: &RefRecord) -> Result<()> {
        store::refs::ref_put(&self.root, rec)
    }

    /// Ref history, newest-first (index 0 = current).
    /// Absent or empty log ⇒ Ok(vec![]). Corrupt entries are skipped silently.
    pub fn ref_log(&self, name: &str) -> Result<Vec<RefRecord>> {
        store::refs::ref_log(&self.root, name)
    }

    /// Enumerate all ref names ever written (from refs-names shards).
    /// Non-UTF-8 name files are skipped. Returns sorted ascending.
    pub fn list_refs(&self) -> Result<Vec<String>> {
        store::refs::list_refs(&self.root)
    }

    // ── imgmeta ──────────────────────────────────────────────────────────────

    /// Store the original OCI image config JSON for `name` (push-fidelity).
    /// The config bytes are content-addressed in the CAS (dedup'd like any
    /// object); the `imgmeta` sidecar records its digest keyed by the ref name,
    /// last-write-wins. `put_bytes` takes its own (shared) write guard, so this
    /// does not nest one. A later `oci push` reads it back via
    /// [`Store::image_config_get`] to re-emit a runnable image.
    pub fn image_config_put(&self, name: &str, config_bytes: &[u8]) -> Result<()> {
        store::imgmeta::image_config_put(&self.root, name, config_bytes)
    }

    /// Read the original OCI image config JSON stored for `name`, if any.
    /// `None` ⇒ no config was captured (a `snapshot`'d ref, or a ref pulled
    /// before push-fidelity shipped) — `oci push` then synthesizes a minimal
    /// config. A corrupt sidecar (not a 32-byte digest) is treated as absent
    /// (fail-soft to the minimal config, never an error).
    pub fn image_config_get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        store::imgmeta::image_config_get(&self.root, name)
    }

    /// R-IMGREC (parity-contract.md §0): store the faithful image manifest
    /// record for `name` (push-fidelity), length-prefixed + content-addressed.
    /// The gc mark-walk extension keeping retained blobs reachable is WP-IMG-01.
    pub fn image_manifest_put(
        &self,
        name: &str,
        rec: &store::imgmeta::ImageManifestRecord,
    ) -> Result<()> {
        store::imgmeta::image_manifest_put(&self.root, name, rec)
    }

    /// R-IMGREC: read the faithful image manifest record for `name`, if any.
    pub fn image_manifest_get(
        &self,
        name: &str,
    ) -> Result<Option<store::imgmeta::ImageManifestRecord>> {
        store::imgmeta::image_manifest_get(&self.root, name)
    }

    /// WP-IMG-09 (R-IMGREC): every CAS digest kept alive by the image sidecars
    /// (`imgmanifest` record blobs + their config/layer descriptors + `imgmeta`
    /// config blobs). The gc mark-walk marks these reachable so it never reaps
    /// blobs retained for a faithful `oci push`. Fail-soft: corrupt/undecodable
    /// sidecars are skipped, never fatal. Order unspecified; may contain dups.
    pub fn list_image_reachable_blobs(&self) -> Result<Vec<Digest>> {
        store::imgmeta::list_image_reachable_blobs(&self.root)
    }

    // ── AC ───────────────────────────────────────────────────────────────────

    /// Read an AC entry.  Absent → Ok(None).
    pub fn ac_get(&self, key: &Digest) -> Result<Option<Vec<u8>>> {
        store::ac::ac_get(&self.root, key)
    }

    /// Write an AC entry atomically (overwrite via temp+rename).
    pub fn ac_put(&self, key: &Digest, value: &[u8]) -> Result<()> {
        store::ac::ac_put(&self.root, key, value)
    }

    /// Enumerate all raw AC values (decoded by caller). Order unspecified.
    pub fn list_ac(&self) -> Result<Vec<Vec<u8>>> {
        store::ac::list_ac(&self.root)
    }
}

// ─────────────────────────────────── tests ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── default_root ─────────────────────────────────────────────────────────

    // Env vars are process-global; these two tests mutate LIGHTR_HOME and
    // race under the parallel test runner — serialize them.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn default_root_honors_lightr_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let orig = std::env::var_os("LIGHTR_HOME");
        std::env::set_var("LIGHTR_HOME", "/tmp/custom-lightr-home");
        let root = Store::default_root();
        // Restore before any assert so we don't leave env dirty on failure.
        match orig {
            Some(v) => std::env::set_var("LIGHTR_HOME", v),
            None => std::env::remove_var("LIGHTR_HOME"),
        }
        assert_eq!(root, PathBuf::from("/tmp/custom-lightr-home/store"));
    }

    #[test]
    fn default_root_fallback_to_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let orig_lightr = std::env::var_os("LIGHTR_HOME");
        std::env::remove_var("LIGHTR_HOME");
        let root = Store::default_root();
        match orig_lightr {
            Some(v) => std::env::set_var("LIGHTR_HOME", v),
            None => std::env::remove_var("LIGHTR_HOME"),
        }
        // Must end with .lightr/store
        assert!(
            root.ends_with(".lightr/store"),
            "expected path ending in .lightr/store, got {:?}",
            root
        );
    }
}
