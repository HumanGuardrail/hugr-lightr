//! Tests for `oci rmi` (WP-IMG-07). Parallel-safe: each test injects its own
//! tempdir store (NO process-global env, NO --test-threads=1).

use super::*;
use lightr_core::{Digest, LightrError, RefRecord};
use lightr_store::{ImageDescriptor, ImageManifestRecord, Store};

fn tmp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

/// Seed a ref + both image sidecars, returning the root.
fn seed_image(store: &Store, name: &str, seed: &[u8]) -> Digest {
    let root = Digest::of_bytes(seed);
    store
        .ref_put(&RefRecord {
            name: name.to_string(),
            root,
            parent: None,
            created_at_unix: 1_700_000_000,
            tool_version: "9.9.9-test".to_string(),
        })
        .unwrap();
    store
        .image_config_put(name, br#"{"config":{"Cmd":["/bin/sh"]}}"#)
        .unwrap();
    store
        .image_manifest_put(
            name,
            &ImageManifestRecord {
                manifest_bytes: b"{\"schemaVersion\":2}".to_vec(),
                descriptors: vec![ImageDescriptor {
                    media_type: "application/vnd.oci.image.layer.v1.tar".to_string(),
                    digest: Digest::of_bytes(b"layer-0"),
                    size: 10,
                }],
                platform: "linux/amd64".to_string(),
            },
        )
        .unwrap();
    root
}

#[test]
fn rmi_removes_ref_and_sidecars() {
    let (_dir, store) = tmp_store();
    seed_image(&store, "@t/img", b"img-root");

    let report = rmi_one(&store, "@t/img", &[], false).unwrap();
    assert_eq!(report.name, "@t/img");
    assert!(!report.forced, "not in use ⇒ not forced");

    // Ref is gone (untagged) and vanishes from list_refs.
    assert!(
        store.ref_get("@t/img").unwrap().is_none(),
        "ref must be gone"
    );
    assert!(
        !store.list_refs().unwrap().iter().any(|r| r == "@t/img"),
        "removed ref must not appear in list_refs"
    );
    // Both sidecars are gone.
    assert!(
        store.image_config_get("@t/img").unwrap().is_none(),
        "config sidecar must be removed"
    );
    assert!(
        store.image_manifest_get("@t/img").unwrap().is_none(),
        "manifest sidecar must be removed"
    );
}

#[test]
fn rmi_absent_ref_is_no_such_image() {
    let (_dir, store) = tmp_store();
    let err = rmi_one(&store, "@t/never", &[], false).unwrap_err();
    assert!(
        matches!(err, LightrError::RefNotFound(_)),
        "absent ref must be RefNotFound (No such image), got {err:?}"
    );
}

#[test]
fn rmi_in_use_refused_without_force() {
    let (_dir, store) = tmp_store();
    seed_image(&store, "@t/busy", b"busy-root");

    let in_use = vec!["@t/busy".to_string()];
    let err = rmi_one(&store, "@t/busy", &in_use, false).unwrap_err();
    assert!(
        matches!(err, LightrError::Io(_)),
        "in-use refusal must be a runtime conflict (exit 1), got {err:?}"
    );
    // The ref must NOT have been removed.
    assert!(
        store.ref_get("@t/busy").unwrap().is_some(),
        "refused rmi must leave the ref intact"
    );
}

#[test]
fn rmi_in_use_removed_with_force() {
    let (_dir, store) = tmp_store();
    seed_image(&store, "@t/busy", b"busy-root");

    let in_use = vec!["@t/busy".to_string()];
    let report = rmi_one(&store, "@t/busy", &in_use, true).unwrap();
    assert!(report.forced, "in-use + -f ⇒ forced=true");
    assert!(
        store.ref_get("@t/busy").unwrap().is_none(),
        "force-removed ref must be gone"
    );
}

#[test]
fn rmi_does_not_touch_other_refs() {
    let (_dir, store) = tmp_store();
    seed_image(&store, "@t/a", b"a");
    seed_image(&store, "@t/b", b"b");

    rmi_one(&store, "@t/a", &[], false).unwrap();
    assert!(store.ref_get("@t/a").unwrap().is_none());
    assert!(
        store.ref_get("@t/b").unwrap().is_some(),
        "rmi of one ref must not disturb another"
    );
}

#[test]
fn rmi_many_continue_on_error() {
    let (_dir, store) = tmp_store();
    seed_image(&store, "@t/ok1", b"ok1");
    seed_image(&store, "@t/ok2", b"ok2");

    // Middle target is absent: the others must still be processed.
    let names = vec![
        "@t/ok1".to_string(),
        "@t/missing".to_string(),
        "@t/ok2".to_string(),
    ];
    let results = rmi_many(&store, &names, &[], false);
    assert_eq!(results.len(), 3, "every target must be reported");

    assert!(matches!(results[0], RmiResult::Removed(_)), "ok1 removed");
    assert!(
        matches!(results[1], RmiResult::Failed { .. }),
        "missing reported as failed"
    );
    assert!(matches!(results[2], RmiResult::Removed(_)), "ok2 removed");

    // The two valid refs are gone despite the failure in between.
    assert!(store.ref_get("@t/ok1").unwrap().is_none());
    assert!(store.ref_get("@t/ok2").unwrap().is_none());
}

#[test]
fn rmi_idempotent_after_remove() {
    let (_dir, store) = tmp_store();
    seed_image(&store, "@t/once", b"once");

    rmi_one(&store, "@t/once", &[], false).unwrap();
    // A second rmi of the now-absent ref is "No such image".
    let err = rmi_one(&store, "@t/once", &[], false).unwrap_err();
    assert!(matches!(err, LightrError::RefNotFound(_)));
}
