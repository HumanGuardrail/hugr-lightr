//! WP-IMG-08 — `image_history` tests. Parallel-safe: each test uses its own
//! tempdir Store (NO process-global env, NO LIGHTR_HOME).

use crate::oci::history::{image_history, HistoryRow, MISSING};
use lightr_core::{Digest, LightrError, RefRecord};
use lightr_store::{ImageDescriptor, ImageManifestRecord, Store};

fn tmp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

fn seed_ref(store: &Store, name: &str) {
    let rec = RefRecord {
        name: name.to_string(),
        root: Digest::of_bytes(name.as_bytes()),
        parent: None,
        created_at_unix: 1_700_000_000,
        tool_version: "9.9.9-test".to_string(),
    };
    store.ref_put(&rec).unwrap();
}

/// Manifest record with `n` layers of the given sizes (descriptor[0] = config).
fn seed_manifest(store: &Store, name: &str, layer_sizes: &[u64]) {
    let mut descriptors = vec![ImageDescriptor {
        media_type: "application/vnd.oci.image.config.v1+json".to_string(),
        digest: Digest::of_bytes(b"config"),
        size: 999,
    }];
    for (i, &size) in layer_sizes.iter().enumerate() {
        descriptors.push(ImageDescriptor {
            media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_string(),
            digest: Digest::of_bytes(format!("layer-{i}").as_bytes()),
            size,
        });
    }
    let rec = ImageManifestRecord {
        manifest_bytes: b"{\"schemaVersion\":2}".to_vec(),
        descriptors,
        platform: "linux/amd64".to_string(),
    };
    store.image_manifest_put(name, &rec).unwrap();
}

#[test]
fn history_lists_layers_with_created_by_and_size() {
    let (_dir, store) = tmp_store();
    seed_ref(&store, "@t/img");
    // Two real layers (1000, 2000) plus one empty (CMD).
    seed_manifest(&store, "@t/img", &[1000, 2000]);
    let config = br#"{
        "history": [
            {"created_by": "ADD file:abc in /"},
            {"created_by": "RUN apt-get install x"},
            {"created_by": "CMD [\"/bin/sh\"]", "empty_layer": true}
        ]
    }"#;
    store.image_config_put("@t/img", config).unwrap();

    let rows = image_history(&store, "@t/img").unwrap();
    assert_eq!(rows.len(), 3, "one row per history entry");

    // Newest-first (docker order): CMD (empty), then RUN, then ADD.
    assert_eq!(
        rows[0],
        HistoryRow {
            created_by: "CMD [\"/bin/sh\"]".to_string(),
            size: Some(0),
            empty_layer: true,
        },
        "empty layer is size 0, newest-first"
    );
    assert_eq!(
        rows[1],
        HistoryRow {
            created_by: "RUN apt-get install x".to_string(),
            size: Some(2000),
            empty_layer: false,
        },
    );
    assert_eq!(
        rows[2],
        HistoryRow {
            created_by: "ADD file:abc in /".to_string(),
            size: Some(1000),
            empty_layer: false,
        },
    );
}

#[test]
fn history_missing_for_no_history_layers() {
    // A ref with retained LAYERS but NO `history` array ⇒ one <missing> row per
    // layer, each with its positional size.
    let (_dir, store) = tmp_store();
    seed_ref(&store, "@t/squashed");
    seed_manifest(&store, "@t/squashed", &[4096, 8192]);
    // Config present but carries no history field.
    store
        .image_config_put("@t/squashed", br#"{"config":{"Cmd":["/bin/sh"]}}"#)
        .unwrap();

    let rows = image_history(&store, "@t/squashed").unwrap();
    assert_eq!(rows.len(), 2, "one row per retained layer");
    for r in &rows {
        assert_eq!(
            r.created_by, MISSING,
            "no history ⇒ created-by is <missing>"
        );
        assert!(!r.empty_layer);
        assert!(r.size.is_some(), "positional size is honest, not None");
    }
    // Newest-first: layer[1] (8192) before layer[0] (4096).
    assert_eq!(rows[0].size, Some(8192));
    assert_eq!(rows[1].size, Some(4096));
}

#[test]
fn history_entry_missing_created_by_renders_missing() {
    let (_dir, store) = tmp_store();
    seed_ref(&store, "@t/partial");
    seed_manifest(&store, "@t/partial", &[100]);
    // One entry with no created_by field at all.
    let config = br#"{"history": [{"comment": "imported"}]}"#;
    store.image_config_put("@t/partial", config).unwrap();

    let rows = image_history(&store, "@t/partial").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].created_by, MISSING, "absent created_by ⇒ <missing>");
    assert_eq!(rows[0].size, Some(100));
}

#[test]
fn history_runs_short_of_layers_is_honest() {
    // More non-empty history entries than retained layers ⇒ the extra rows
    // report size None (rendered <missing>), never a fabricated zero.
    let (_dir, store) = tmp_store();
    seed_ref(&store, "@t/short");
    seed_manifest(&store, "@t/short", &[500]); // only one layer
    let config = br#"{
        "history": [
            {"created_by": "FROM scratch"},
            {"created_by": "ADD second layer"}
        ]
    }"#;
    store.image_config_put("@t/short", config).unwrap();

    let rows = image_history(&store, "@t/short").unwrap();
    assert_eq!(rows.len(), 2);
    // Newest-first: the SECOND entry consumed no layer (descriptors exhausted).
    assert_eq!(rows[0].created_by, "ADD second layer");
    assert_eq!(rows[0].size, None, "no descriptor left ⇒ size unknown");
    assert_eq!(rows[1].created_by, "FROM scratch");
    assert_eq!(rows[1].size, Some(500));
}

#[test]
fn history_absent_ref_is_ref_not_found() {
    let (_dir, store) = tmp_store();
    let err = image_history(&store, "@t/never").unwrap_err();
    assert!(
        matches!(err, LightrError::RefNotFound(_)),
        "absent ref must be RefNotFound (exit 2), got {err:?}"
    );
}

#[test]
fn history_empty_name_is_fail_closed() {
    // An empty name resolves to NO image — fail-closed. The store's ref lookup
    // rejects it as `InvalidRef` (an empty name is structurally invalid); both
    // InvalidRef and RefNotFound map to exit 2 at the CLI, so either is correct
    // honesty — what matters is it is an ERROR, never a silent empty table.
    let (_dir, store) = tmp_store();
    let err = image_history(&store, "").unwrap_err();
    assert!(
        matches!(
            err,
            LightrError::RefNotFound(_) | LightrError::InvalidRef(_)
        ),
        "empty ref must be a fail-closed error (exit 2), got {err:?}"
    );
}

#[test]
fn history_ref_without_provenance_errors() {
    // A ref that exists but has neither config nor manifest record ⇒ honest
    // InvalidManifest, never a silent empty table.
    let (_dir, store) = tmp_store();
    seed_ref(&store, "@t/bare");
    let err = image_history(&store, "@t/bare").unwrap_err();
    assert!(
        matches!(err, LightrError::InvalidManifest(_)),
        "no provenance must be InvalidManifest, got {err:?}"
    );
}
