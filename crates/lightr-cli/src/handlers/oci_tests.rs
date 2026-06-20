//! Tests for the oci handlers — split via #[path] to keep oci.rs under the
//! 400-LOC godfile cap (CMP/IMG verb tests live here).

// Handler-level unit checks — no network; clap parse tests are in main.rs.

#[test]
fn import_bad_ref_exits_2() {
    // Uppercase name is invalid ref
    let code = super::import("/some/path", "INVALID", false);
    assert_eq!(code, 2, "bad ref name must exit 2");
}

#[test]
fn import_empty_ref_exits_2() {
    let code = super::import("/some/path", "", false);
    assert_eq!(code, 2, "empty ref name must exit 2");
}

#[test]
fn pull_bad_ref_exits_2() {
    let code = super::pull_image("alpine", "INVALID", false);
    assert_eq!(code, 2, "bad ref name must exit 2");
}

#[test]
fn pull_empty_ref_exits_2() {
    let code = super::pull_image("alpine", "", false);
    assert_eq!(code, 2, "empty ref name must exit 2");
}

#[test]
fn push_bad_ref_exits_2() {
    // Uppercase store-ref is an invalid ref name → exit 2.
    let code = super::push_image("INVALID", "localhost:5000/x:latest", false);
    assert_eq!(code, 2, "bad store-ref name must exit 2");
}

#[test]
fn push_empty_ref_exits_2() {
    let code = super::push_image("", "localhost:5000/x:latest", false);
    assert_eq!(code, 2, "empty store-ref name must exit 2");
}

#[test]
fn push_unknown_ref_exits_2() {
    // Valid name but absent ref → RefNotFound → exit 2 (no network touched).
    // Uses an isolated LIGHTR_HOME so it never hits the user's real store.
    let _guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let code = super::push_image("@t/never-pushed", "localhost:5000/x:latest", false);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 2, "unknown ref must exit 2 (RefNotFound)");
}

// ── WP-IMG-03: oci tag ─────────────────────────────────────────────────────
//
// The behavioural tests drive `tag_in_store` with an injected tempdir Store
// (parallel-safe — NO process-global env). The exit-code tests cover the
// thin public `tag` wrapper's name validation (no store needed).

use lightr_core::{Digest, RefRecord};
use lightr_store::{ImageDescriptor, ImageManifestRecord, Store};

fn tmp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

fn seed_ref(store: &Store, name: &str, seed: &[u8]) -> Digest {
    let root = Digest::of_bytes(seed);
    let rec = RefRecord {
        name: name.to_string(),
        root,
        parent: None,
        created_at_unix: 1_700_000_000,
        tool_version: "9.9.9-test".to_string(),
    };
    store.ref_put(&rec).unwrap();
    root
}

#[test]
fn tag_bad_src_name_exits_2() {
    // Invalid src name → exit 2 (validated before any store open).
    let code = super::tag("INVALID", "@t/dst");
    assert_eq!(code, 2, "bad src name must exit 2");
}

#[test]
fn tag_bad_target_name_exits_2() {
    let code = super::tag("@t/src", "INVALID");
    assert_eq!(code, 2, "bad target name must exit 2");
}

#[test]
fn tag_aliases_dst_to_same_root() {
    let (_dir, store) = tmp_store();
    let root = seed_ref(&store, "@t/src", b"img-root");

    super::tag_in_store(&store, "@t/src", "@t/dst").unwrap();

    let dst = store.ref_get("@t/dst").unwrap().expect("dst must exist");
    assert_eq!(
        dst.root, root,
        "alias must point at the same root (zero copy)"
    );
    // src is untouched.
    let src = store.ref_get("@t/src").unwrap().unwrap();
    assert_eq!(src.root, root, "src must be unchanged");
}

#[test]
fn tag_src_absent_is_error() {
    let (_dir, store) = tmp_store();
    // No src seeded → fail-closed RefNotFound.
    let err = super::tag_in_store(&store, "@t/never", "@t/dst").unwrap_err();
    assert!(
        matches!(err, lightr_core::LightrError::RefNotFound(_)),
        "absent src must be RefNotFound, got {err:?}"
    );
    // And nothing got written for dst.
    assert!(
        store.ref_get("@t/dst").unwrap().is_none(),
        "no alias may be written when src is absent"
    );
}

#[test]
fn tag_copies_sidecars() {
    let (_dir, store) = tmp_store();
    seed_ref(&store, "@t/src", b"img-with-sidecars");

    // IMG-01 sidecars on src: config + manifest record.
    let config = br#"{"config":{"Cmd":["/bin/sh"]}}"#;
    store.image_config_put("@t/src", config).unwrap();
    let rec = ImageManifestRecord {
        manifest_bytes: b"{\"schemaVersion\":2}".to_vec(),
        descriptors: vec![ImageDescriptor {
            media_type: "application/vnd.oci.image.layer.v1.tar".to_string(),
            digest: Digest::of_bytes(b"layer-0"),
            size: 1234,
        }],
        platform: "linux/amd64".to_string(),
    };
    store.image_manifest_put("@t/src", &rec).unwrap();

    super::tag_in_store(&store, "@t/src", "@t/dst").unwrap();

    // Both sidecar families must now be present + identical on dst.
    let dst_config = store
        .image_config_get("@t/dst")
        .unwrap()
        .expect("config sidecar must be copied to dst");
    assert_eq!(dst_config, config, "copied config must match src");
    let dst_manifest = store
        .image_manifest_get("@t/dst")
        .unwrap()
        .expect("manifest sidecar must be copied to dst");
    assert_eq!(dst_manifest, rec, "copied manifest record must match src");
}

#[test]
fn tag_no_sidecar_still_works() {
    // A ref with NO sidecars (e.g. a snapshot'd ref) tags fine.
    let (_dir, store) = tmp_store();
    let root = seed_ref(&store, "@t/plain", b"no-sidecars");

    super::tag_in_store(&store, "@t/plain", "@t/plain-alias").unwrap();

    let dst = store.ref_get("@t/plain-alias").unwrap().unwrap();
    assert_eq!(dst.root, root);
    assert!(
        store.image_config_get("@t/plain-alias").unwrap().is_none(),
        "no sidecar on src ⇒ none on dst"
    );
}

#[test]
fn tag_lww_repoints_existing_dst() {
    let (_dir, store) = tmp_store();
    let root_a = seed_ref(&store, "@t/a", b"root-a");
    let root_b = seed_ref(&store, "@t/b", b"root-b");

    // First tag dst→a, then re-tag dst→b (last-write-wins).
    super::tag_in_store(&store, "@t/a", "@t/dst").unwrap();
    let first = store.ref_get("@t/dst").unwrap().unwrap();
    assert_eq!(first.root, root_a, "first tag must point at a");

    super::tag_in_store(&store, "@t/b", "@t/dst").unwrap();
    let second = store.ref_get("@t/dst").unwrap().unwrap();
    assert_eq!(second.root, root_b, "re-tag must repoint dst (LWW)");
}

// ── WP-IMG-06: oci images ──────────────────────────────────────────────────
//
// The substantive behaviour (repo/tag/id/size, unique-object size, sorting)
// is covered parallel-safely in lightr-oci's images_tests against an injected
// store. Here we only smoke the handler's default-root wiring + exit code on an
// empty store, which requires LIGHTR_HOME ⇒ ENV_LOCK serialization.

#[test]
fn images_empty_store_exits_0() {
    let _guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let code_table = super::images(false);
    let code_json = super::images(true);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code_table, 0, "empty store table → exit 0 (header only)");
    assert_eq!(code_json, 0, "empty store json → exit 0 ([])");
}

// ── WP-IMG-07: oci rmi ──────────────────────────────────────────────────────
//
// Substantive behaviour (untag + sidecars, in-use guard, multi continue-on-
// error) is covered parallel-safely in lightr-oci's rmi_tests against an
// injected store. Here we only smoke the handler's default-root wiring + exit
// code on an empty store (absent ref ⇒ No such image ⇒ 2), which requires
// LIGHTR_HOME ⇒ ENV_LOCK serialization.

#[test]
fn rmi_absent_ref_exits_2() {
    let _guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let code = super::rmi(&["@t/never".to_string()], false);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 2, "absent ref → No such image → exit 2");
}

// ── WP-IMG-08: oci history ────────────────────────────────────────────────────
//
// Substantive behaviour (created-by/size/<missing>, newest-first, no-provenance
// error) is covered parallel-safely in lightr-oci's history_tests against an
// injected store. Here we only smoke the handler's name-validation + default-root
// wiring: a bad ref exits 2 before any store open; an absent (but valid) ref
// exits 2 via RefNotFound (requires LIGHTR_HOME ⇒ ENV_LOCK serialization).

#[test]
fn history_bad_ref_exits_2() {
    let code = super::history("INVALID", false);
    assert_eq!(code, 2, "bad ref name must exit 2 (no store opened)");
}

#[test]
fn history_absent_ref_exits_2() {
    let _guard = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::TempDir::new().unwrap();
    std::env::set_var("LIGHTR_HOME", tmp.path());
    let code = super::history("@t/never", false);
    std::env::remove_var("LIGHTR_HOME");
    assert_eq!(code, 2, "absent ref → RefNotFound → exit 2");
}
