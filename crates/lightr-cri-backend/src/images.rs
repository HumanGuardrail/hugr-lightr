//! Image plane — pull / status / list / remove / fs_info (WP-CRI-MVP).
//!
//! WIRING: the bytes live in the real hugr-lightr CAS. `pull_image` delegates to
//! `lightr_oci::pull` (OCI dist v2, lazy CAS — pull moves layer bytes into the
//! store and tags a ref; the seam's "pull_image MUST NOT move file bytes" lazy
//! law is the store's CoW concern, honored by the engine). `image_fs_info`
//! reads `Store::store_usage`; `remove_image` calls `lightr_oci::rmi_one` (with
//! an in-use guard from live containers); `list_images`/`image_status` read the
//! store's refs enriched by `lightr_oci::list_images` for size.
//!
//! CRI ↔ store name mapping: a CRI image_ref (`busybox:latest`) is NOT a valid
//! lightr ref name (ADR-0004 forbids `:` and `/`), so it is SANITIZED to a store
//! name (`:`/`/` → `-`). A small CRI sidecar under `<home>/cri/images/` records
//! the original image_ref ↔ store name ↔ image id so the seam returns the
//! caller's original ref (matching the fake's record model) while the bytes and
//! sizing come from the real store. Crash-only: the sidecar is written before
//! `pull_image` returns and removed after a successful `rmi`.

use std::fs;

use crate::util::{atomic_write_json, map_lightr_err, now_nanos};
use crate::vocab::{BackendError, ContainerState, FsInfo, ImageRecord, PulledImage, Result};
use crate::LightrBackend;
use lightr_store::Store;

/// CRI image sidecar: maps the caller's original image_ref to the store name
/// the bytes are tagged under, plus the resolved id/size at pull time.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct CriImageRecord {
    /// Caller's original CRI image_ref (e.g. `busybox:latest`).
    image_ref: String,
    /// Sanitized lightr store ref name the bytes are tagged under.
    store_name: String,
    /// Image id (short root hex).
    id: String,
    /// Size in bytes (unique reachable CAS objects) at pull time.
    size: u64,
}

/// Sanitize a CRI image_ref into a valid lightr ref name (ADR-0004 grammar):
/// `:`/`/` → `-`, lowercased. Deterministic so the same ref maps to the same
/// store name across calls. `pub(crate)` so the container plane (WP-#99 hydrate)
/// maps an image_ref to the SAME store name the pull tagged the bytes under.
pub(crate) fn sanitize_ref(image_ref: &str) -> String {
    image_ref
        .chars()
        .map(|c| match c {
            ':' | '/' => '-',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

impl LightrBackend {
    fn store(&self) -> Result<Store> {
        Store::open(self.home().join("store")).map_err(map_lightr_err)
    }

    fn load_cri_images(&self) -> Vec<CriImageRecord> {
        let dir = self.images_dir();
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(data) = fs::read(&path) {
                    if let Ok(rec) = serde_json::from_slice::<CriImageRecord>(&data) {
                        out.push(rec);
                    }
                }
            }
        }
        out
    }

    // ── pull ─────────────────────────────────────────────────────────────────

    pub(crate) fn pull_image_impl(&self, image_ref: &str) -> Result<PulledImage> {
        // Transcribed validation: reject empty/whitespace refs (the engine's
        // parser also rejects malformed refs with InvalidRef → mapped here).
        if image_ref.is_empty() || image_ref.chars().any(|c| c.is_ascii_whitespace()) {
            return Err(BackendError::InvalidArgument(format!(
                "image_ref {image_ref:?} is empty or contains whitespace"
            )));
        }
        let store_name = sanitize_ref(image_ref);
        let store = self.store()?;
        let report = lightr_oci::pull(image_ref, &store, &store_name).map_err(map_lightr_err)?;
        let root_hex = report.root.to_hex();

        // Size = unique reachable CAS objects for this ref (engine's listing).
        let size = lightr_oci::list_images(&store)
            .map_err(map_lightr_err)?
            .into_iter()
            .find(|r| r.digest == root_hex)
            .map(|r| r.size)
            .unwrap_or(0);

        let id = short_hex(&root_hex);
        let sidecar = CriImageRecord {
            image_ref: image_ref.to_string(),
            store_name,
            id,
            size,
        };
        // Crash-only: persist the CRI sidecar before returning.
        let fname = format!("{}.json", sanitize_ref(image_ref));
        atomic_write_json(&self.images_dir(), &fname, &sidecar)?;

        Ok(PulledImage {
            ref_name: image_ref.to_string(),
            root_hex,
            total_size: size,
        })
    }

    // ── status / list ────────────────────────────────────────────────────────

    pub(crate) fn image_status_impl(&self, image_ref: &str) -> Result<Option<ImageRecord>> {
        Ok(self
            .load_cri_images()
            .into_iter()
            .find(|r| r.image_ref == image_ref)
            .map(|r| ImageRecord {
                id: r.id,
                ref_name: r.image_ref,
                size: r.size,
            }))
    }

    pub(crate) fn list_images_impl(&self) -> Result<Vec<ImageRecord>> {
        let mut rows: Vec<ImageRecord> = self
            .load_cri_images()
            .into_iter()
            .map(|r| ImageRecord {
                id: r.id,
                ref_name: r.image_ref,
                size: r.size,
            })
            .collect();
        rows.sort_by(|a, b| a.ref_name.cmp(&b.ref_name));
        Ok(rows)
    }

    // ── remove (idempotent; InUse while referenced by a live container) ──────

    pub(crate) fn remove_image_impl(&self, image_ref: &str) -> Result<()> {
        let sidecar = self
            .load_cri_images()
            .into_iter()
            .find(|r| r.image_ref == image_ref);
        let Some(sidecar) = sidecar else {
            return Ok(()); // idempotent: not-found → Ok (CRI law)
        };

        // InUse guard: refuse while a non-Exited container references this ref
        // (transcribed from the fake's reason).
        {
            let cache = self.cache.lock().unwrap();
            for c in cache.containers.values() {
                if c.config.image_ref == image_ref && c.state != ContainerState::Exited {
                    return Err(BackendError::InUse(format!(
                        "image {image_ref} referenced by container {}",
                        c.id.0
                    )));
                }
            }
        }

        // Untag in the real store (gc reclaims the blobs). rmi_one errors on a
        // genuinely-absent ref; treat that as already-gone (idempotent).
        let store = self.store()?;
        match lightr_oci::rmi_one(&store, &sidecar.store_name, &[], true) {
            Ok(_) => {}
            Err(lightr_core::LightrError::RefNotFound(_)) => {}
            Err(e) => return Err(map_lightr_err(e)),
        }

        let fname = format!("{}.json", sanitize_ref(image_ref));
        let _ = fs::remove_file(self.images_dir().join(fname));
        Ok(())
    }

    // ── fs_info (store sizing) ───────────────────────────────────────────────

    pub(crate) fn image_fs_info_impl(&self) -> Result<FsInfo> {
        let store = self.store()?;
        let usage = store.store_usage().map_err(map_lightr_err)?;
        Ok(FsInfo {
            timestamp_nanos: now_nanos(),
            mountpoint: self.home().join("store").display().to_string(),
            used_bytes: usage.bytes,
            inodes_used: usage.objects,
        })
    }
}

/// 12-char short hex docker/CRI prints for an image id (full root hex is 64).
fn short_hex(full_hex: &str) -> String {
    full_hex.chars().take(12).collect()
}
