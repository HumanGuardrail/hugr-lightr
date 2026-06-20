//! WP-IMG-01 — retention roundtrip / digest-mismatch / idempotency tests.
//!
//! Pull retention is exercised through the import paths (layout + docker-save),
//! which share the same `retain_image_manifest` core and run fully local (no
//! network). Each test injects its own tempdir store (no global env mutation
//! beyond the existing ENV_LOCK that snapshot/hydrate require).

use super::{make_layer, make_layout, make_modern_docker_save, tmp_store_and_home, ENV_LOCK};
use crate::oci::import::import_layout;
use std::fs;
use tempfile::TempDir;

/// Retention roundtrip: an OCI-layout import retains the manifest + one
/// descriptor per blob (config first, then layers), and every retained
/// descriptor's CAS digest resolves back to the original raw blob bytes.
#[test]
fn retain_layout_roundtrip_faithful_record() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let layer1 = make_layer(&[("bin/", &[], 0o755), ("bin/a", b"alpha", 0o755)]);
    let layer2 = make_layer(&[("bin/b", b"bravo", 0o644)]);
    let layout_dir = make_layout(tmp.path(), &[layer1.clone(), layer2.clone()]);

    import_layout(&layout_dir, &store, "retain-img").unwrap();

    let rec = store
        .image_manifest_get("retain-img")
        .unwrap()
        .expect("a faithful manifest record must be retained at import");

    // make_layout writes a zero-size config descriptor (digest all-zero, not a
    // real sha256 of the config blob) — `sha256_hex(config.digest)` is Some but
    // the config blob at blobs/sha256/<all-zero> does not exist, so config
    // retention is skipped (best-effort). The two layers ARE retained, in order.
    assert_eq!(
        rec.descriptors.len(),
        2,
        "two layer descriptors retained (config skipped: synthetic layout config)"
    );

    // Original manifest JSON retained verbatim (the blob at blobs/sha256/<hex>).
    assert!(
        !rec.manifest_bytes.is_empty(),
        "the original manifest bytes must be retained"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&rec.manifest_bytes).unwrap();
    assert_eq!(parsed["schemaVersion"], 2);

    // Each retained descriptor's CAS digest resolves to the EXACT raw blob.
    let got0 = store.get_bytes(&rec.descriptors[0].digest).unwrap();
    let got1 = store.get_bytes(&rec.descriptors[1].digest).unwrap();
    assert_eq!(got0, layer1, "layer 1 raw bytes retained byte-for-byte");
    assert_eq!(got1, layer2, "layer 2 raw bytes retained byte-for-byte");
    // Sizes mirror the original blobs.
    assert_eq!(rec.descriptors[0].size, layer1.len() as u64);
    assert_eq!(rec.descriptors[1].size, layer2.len() as u64);
    // Ordered media types preserved.
    assert_eq!(
        rec.descriptors[0].media_type,
        "application/vnd.oci.image.layer.v1.tar+gzip"
    );
}

/// Idempotent re-import: importing the same layout twice leaves a single,
/// identical retained record (last-write-wins, no corruption).
#[test]
fn retain_layout_idempotent_reimport() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let layer = make_layer(&[("f", b"data", 0o644)]);
    let layout_dir = make_layout(tmp.path(), &[layer]);

    import_layout(&layout_dir, &store, "idem-img").unwrap();
    let rec1 = store.image_manifest_get("idem-img").unwrap().unwrap();
    import_layout(&layout_dir, &store, "idem-img").unwrap();
    let rec2 = store.image_manifest_get("idem-img").unwrap().unwrap();

    assert_eq!(rec1, rec2, "re-import must produce an identical record");
}

/// Modern docker-save retention: the docker-save manifest.json is retained
/// verbatim, the config (digest checks out) + the layer are retained, and the
/// platform is parsed from the config JSON (`linux/amd64`).
#[test]
fn retain_docker_save_modern_roundtrip() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let mut layer_tar = Vec::new();
    {
        let mut t = tar::Builder::new(&mut layer_tar);
        let content = b"modern retain\n";
        let mut h = tar::Header::new_gnu();
        h.set_path("usr/bin/x").unwrap();
        h.set_mode(0o755);
        h.set_size(content.len() as u64);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        t.append(&h, &content[..]).unwrap();
        t.finish().unwrap();
    }
    let tar_path = tmp.path().join("modern.tar");
    fs::write(&tar_path, make_modern_docker_save(&layer_tar, false)).unwrap();

    import_layout(&tar_path, &store, "ds-img").unwrap();

    let rec = store.image_manifest_get("ds-img").unwrap().unwrap();
    // make_modern_docker_save emits a real-digest config + one layer ⇒ both
    // retained (config first).
    assert_eq!(rec.descriptors.len(), 2, "config + one layer retained");
    assert_eq!(
        rec.platform, "linux/amd64",
        "platform parsed from the config JSON"
    );
    // The layer descriptor's CAS digest resolves to the original layer tar.
    let layer_back = store.get_bytes(&rec.descriptors[1].digest).unwrap();
    assert_eq!(layer_back, layer_tar, "layer retained byte-for-byte");
    // The retained manifest is the docker-save manifest.json (a JSON array).
    let parsed: serde_json::Value = serde_json::from_slice(&rec.manifest_bytes).unwrap();
    assert!(parsed.is_array(), "docker-save manifest.json is an array");
}

/// Verify-then-retain is fail-closed: a modern docker-save blob whose content
/// does not match its `blobs/sha256/<digest>` path is rejected — NO record is
/// silently retained.
#[test]
fn retain_rejects_digest_mismatch() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    let mut layer_tar = Vec::new();
    {
        let mut t = tar::Builder::new(&mut layer_tar);
        let content = b"tampered\n";
        let mut h = tar::Header::new_gnu();
        h.set_path("x").unwrap();
        h.set_mode(0o644);
        h.set_size(content.len() as u64);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        t.append(&h, &content[..]).unwrap();
        t.finish().unwrap();
    }
    let tar_path = tmp.path().join("bad.tar");
    fs::write(&tar_path, make_modern_docker_save(&layer_tar, true)).unwrap();

    let res = import_layout(&tar_path, &store, "bad-img");
    assert!(res.is_err(), "a digest mismatch must be rejected");
    // Fail-closed: no faithful record is retained on a rejected import.
    assert!(
        store.image_manifest_get("bad-img").unwrap().is_none(),
        "no record may be silently retained on digest mismatch"
    );
}
