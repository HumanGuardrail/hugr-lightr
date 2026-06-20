//! WP-IMG-09 (R-IMGREC) — gc reachability for retained image blobs.
//!
//! IMG-01 retains the original config + layer blobs in the CAS, referenced ONLY
//! by the `imgmanifest` sidecar. The gc mark-walk must enumerate that sidecar
//! and mark the retained blobs (record + config + each layer) reachable, else
//! gc reaps them and a future faithful `oci push` loses layers.
//!
//! Parallel-safe: each test injects its OWN tempdir store root and mutates NO
//! process-global state (no `LIGHTR_HOME`/cwd/env). The assertions are purely
//! about object blobs (survive/reaped), which are independent of run-dir pruning
//! — so the unset-LIGHTR_HOME fallback cannot affect them.
#![cfg(test)]

use crate::index::gc::gc;
use lightr_store::{ImageDescriptor, ImageManifestRecord, Store};
use tempfile::TempDir;

/// Simulate IMG-01 retention: put raw config + layer blobs into the CAS and
/// record a faithful manifest that references them by digest. Run gc → the
/// retained blobs (record + config + each layer) SURVIVE; an unreferenced
/// non-retained orphan blob is still reaped.
#[test]
fn retained_image_blobs_survive_gc_orphan_reaped() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();

    // ── Simulate IMG-01: retain the original config + layer blobs in the CAS.
    // These live in the CAS referenced ONLY by the imgmanifest sidecar below.
    let config_bytes = br#"{"architecture":"amd64","os":"linux"}"#;
    let layer0_bytes = b"layer-0-raw-tar-gz-blob-contents";
    let layer1_bytes = b"layer-1-raw-tar-gz-blob-contents-distinct";

    let config_digest = store.put_bytes(config_bytes).unwrap();
    let layer0_digest = store.put_bytes(layer0_bytes).unwrap();
    let layer1_digest = store.put_bytes(layer1_bytes).unwrap();

    // The faithful manifest record references those retained blobs by digest.
    let rec = ImageManifestRecord {
        manifest_bytes: br#"{"schemaVersion":2,"layers":[]}"#.to_vec(),
        descriptors: vec![
            ImageDescriptor {
                media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                digest: config_digest,
                size: config_bytes.len() as u64,
            },
            ImageDescriptor {
                media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_string(),
                digest: layer0_digest,
                size: layer0_bytes.len() as u64,
            },
            ImageDescriptor {
                media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_string(),
                digest: layer1_digest,
                size: layer1_bytes.len() as u64,
            },
        ],
        platform: "linux/amd64".to_string(),
    };
    store.image_manifest_put("retained-img", &rec).unwrap();

    // ── A non-retained orphan blob: referenced by NO ref/AC/sidecar.
    let orphan_bytes = b"orphan-blob-no-sidecar-references-this";
    let orphan_digest = store.put_bytes(orphan_bytes).unwrap();

    // Sanity: everything exists pre-gc.
    assert!(store.exists(&config_digest), "config blob pre-gc");
    assert!(store.exists(&layer0_digest), "layer0 blob pre-gc");
    assert!(store.exists(&layer1_digest), "layer1 blob pre-gc");
    assert!(store.exists(&orphan_digest), "orphan blob pre-gc");

    // ── Real gc sweep (force, min_age=0).
    let report = gc(&store, false, 0).unwrap();

    // The retained config + layer blobs MUST survive (marked reachable via the
    // imgmanifest sidecar walk — WP-IMG-09).
    assert!(
        store.exists(&config_digest),
        "retained config blob must survive gc"
    );
    assert!(
        store.exists(&layer0_digest),
        "retained layer-0 blob must survive gc"
    );
    assert!(
        store.exists(&layer1_digest),
        "retained layer-1 blob must survive gc"
    );

    // The unreferenced orphan MUST be reaped (gc still works normally).
    assert!(
        !store.exists(&orphan_digest),
        "unreferenced non-retained orphan blob must be reaped"
    );
    assert!(
        report.swept >= 1,
        "gc must report ≥1 swept object (the orphan), got {}",
        report.swept
    );
}

/// The encoded record blob itself (pointed at by the imgmanifest sidecar, not by
/// any descriptor) must also survive — else a later read-back of the record
/// fails and faithful push cannot reconstruct the manifest.
#[test]
fn retained_record_blob_itself_survives_gc() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();

    let layer_bytes = b"only-layer-blob";
    let layer_digest = store.put_bytes(layer_bytes).unwrap();

    let rec = ImageManifestRecord {
        manifest_bytes: b"{}".to_vec(),
        descriptors: vec![ImageDescriptor {
            media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_string(),
            digest: layer_digest,
            size: layer_bytes.len() as u64,
        }],
        platform: String::new(),
    };
    store.image_manifest_put("rec-img", &rec).unwrap();

    // Run gc, then prove the record is still readable end-to-end (record blob +
    // layer blob both survived).
    gc(&store, false, 0).unwrap();

    let got = store.image_manifest_get("rec-img").unwrap();
    assert_eq!(
        got.as_ref(),
        Some(&rec),
        "record must read back identically after gc (record blob survived)"
    );
    assert!(
        store.exists(&layer_digest),
        "the layer blob the record references must survive gc"
    );
}

/// A captured `imgmeta` config sidecar blob must also survive gc.
#[test]
fn retained_image_config_sidecar_blob_survives_gc() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();

    let cfg = br#"{"config":{"Cmd":["sh"]}}"#;
    store.image_config_put("cfg-img", cfg).unwrap();

    // Add an orphan to ensure gc actually sweeps something this run.
    let orphan = store.put_bytes(b"orphan-for-config-test").unwrap();

    gc(&store, false, 0).unwrap();

    assert!(
        store.image_config_get("cfg-img").unwrap().as_deref() == Some(&cfg[..]),
        "captured image-config blob must survive gc and read back identically"
    );
    assert!(!store.exists(&orphan), "orphan must still be reaped");
}
