//! Tests for `imgmeta` (image config + manifest-record sidecars) — split out of
//! `imgmeta.rs` via `#[path]` to keep both files under the 400-LOC godfile cap.

use crate::Store;
use tempfile::TempDir;

fn tmp_store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

// ── image_config sidecar (push-fidelity) ──────────────────────────────────

#[test]
fn image_config_roundtrip_and_absent_is_none() {
    let (_dir, store) = tmp_store();
    // A ref with no captured config ⇒ None (push then synthesizes minimal).
    assert!(store.image_config_get("noconfig").unwrap().is_none());
    // Put + get roundtrips the exact bytes (content-addressed in the CAS).
    let cfg = br#"{"architecture":"amd64","os":"linux","config":{"Cmd":["sh"]}}"#;
    store.image_config_put("img", cfg).unwrap();
    assert_eq!(
        store.image_config_get("img").unwrap().as_deref(),
        Some(&cfg[..])
    );
    // Last-write-wins: a second put replaces the sidecar.
    let cfg2 = br#"{"os":"linux"}"#;
    store.image_config_put("img", cfg2).unwrap();
    assert_eq!(
        store.image_config_get("img").unwrap().as_deref(),
        Some(&cfg2[..])
    );
}

// ── R-IMGREC: image manifest record codec roundtrip ───────────────────────

#[test]
fn image_manifest_record_roundtrip_and_absent_is_none() {
    use crate::store::imgmeta::{ImageDescriptor, ImageManifestRecord};
    use lightr_core::Digest;

    let (_dir, store) = tmp_store();
    // Absent ⇒ None.
    assert!(store.image_manifest_get("nomani").unwrap().is_none());

    let rec = ImageManifestRecord {
        manifest_bytes: br#"{"schemaVersion":2,"layers":[]}"#.to_vec(),
        descriptors: vec![
            ImageDescriptor {
                media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                digest: Digest([1u8; 32]),
                size: 123,
            },
            ImageDescriptor {
                media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_string(),
                digest: Digest([2u8; 32]),
                size: 456789,
            },
        ],
        platform: "linux/amd64".to_string(),
    };
    store.image_manifest_put("mani", &rec).unwrap();
    let got = store.image_manifest_get("mani").unwrap().unwrap();
    assert_eq!(got, rec, "record must survive the length-prefixed codec");

    // Last-write-wins: a second put replaces the sidecar.
    let rec2 = ImageManifestRecord {
        manifest_bytes: b"{}".to_vec(),
        descriptors: vec![],
        platform: String::new(),
    };
    store.image_manifest_put("mani", &rec2).unwrap();
    assert_eq!(store.image_manifest_get("mani").unwrap().unwrap(), rec2);
}
