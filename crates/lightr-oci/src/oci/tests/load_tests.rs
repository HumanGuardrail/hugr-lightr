//! WP-IMG-05 — `oci load` tests: save→load lossless roundtrip, docker-save tar
//! load, RepoTags naming + fallback, sanitize, and fail-closed malformed input.
//!
//! Parallel-safe: every test injects its own tempdir store (NO process-global
//! env, NO stdin). The stdin path of `load` is a thin `read_to_end` wrapper over
//! the same in-memory buffer the `-i` tests exercise.

use crate::oci::import::import_layout;
use crate::oci::load::{first_repo_tag, load, sanitize_ref};
use crate::oci::save::save;
use crate::oci::tests::{make_layer, make_layout, make_modern_docker_save};
use lightr_core::LightrError;
use lightr_store::Store;
use std::fs;
use tempfile::TempDir;

/// A store under its own tempdir (NO global env — parallel-safe).
fn tmp_store() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path().join("store")).unwrap();
    (dir, store)
}

/// `load(save(x))` is LOSSLESS: import an OCI layout into store A (retains a
/// faithful record), `save` it to a tar, `load` that tar into a fresh store B,
/// and assert the same root + the same retained blob digests survive.
#[test]
fn save_load_roundtrip_lossless() {
    let (dir_a, store_a) = tmp_store();

    let layer1 = make_layer(&[("bin/", &[], 0o755), ("bin/a", b"alpha", 0o755)]);
    let layer2 = make_layer(&[("bin/b", b"bravo", 0o644)]);
    let layout_dir = make_layout(dir_a.path(), &[layer1.clone(), layer2.clone()]);
    let imported = import_layout(&layout_dir, &store_a, "img").unwrap();
    let rec_a = store_a.image_manifest_get("img").unwrap().unwrap();

    // save → tar.
    let out = dir_a.path().join("out.tar");
    save("img", Some(&out), &store_a).unwrap();

    // load the tar into a fresh store B.
    let (_dir_b, store_b) = tmp_store();
    let report = load(Some(&out), &store_b).unwrap();

    // Same root digest after the roundtrip (same content tree).
    assert_eq!(
        report.root, imported.root,
        "save→load reproduces the same root digest"
    );

    // The loaded ref exists in store B under the reported name + points at root.
    let loaded = store_b
        .ref_get(&report.name)
        .unwrap()
        .expect("loaded ref must exist");
    assert_eq!(loaded.root, report.root, "loaded ref points at the root");

    // Same blob digests: every original layer blob survives byte-for-byte.
    let rec_b = store_b.image_manifest_get(&report.name).unwrap().unwrap();
    let digests_a: std::collections::HashSet<_> =
        rec_a.descriptors.iter().map(|d| d.digest).collect();
    for d in &rec_b.descriptors {
        assert!(
            digests_a.contains(&d.digest),
            "re-loaded blob digest must match an original"
        );
    }
    let bodies_b: std::collections::HashSet<_> = rec_b
        .descriptors
        .iter()
        .map(|d| store_b.get_bytes(&d.digest).unwrap())
        .collect();
    assert!(bodies_b.contains(&layer1), "layer 1 byte-for-byte");
    assert!(bodies_b.contains(&layer2), "layer 2 byte-for-byte");
}

/// `load` of a modern `docker save` tar (OCI-layout export with `RepoTags`) tags
/// the image under the sanitized RepoTag and reports `tagged_from_tar = true`.
#[test]
fn load_docker_save_tar_tags_from_repo_tags() {
    let (dir, store) = tmp_store();
    let layer = make_layer(&[("file.txt", b"content", 0o644)]);
    let tar = make_modern_docker_save(&layer, false);
    let tar_path = dir.path().join("docker-save.tar");
    fs::write(&tar_path, &tar).unwrap();

    let report = load(Some(&tar_path), &store).unwrap();

    assert!(report.tagged_from_tar, "RepoTags present ⇒ tagged from tar");
    // RepoTags = ["modern:latest"] → @loaded/modern-latest.
    assert_eq!(report.name, "@loaded/modern-latest");
    assert_eq!(report.layers, 1, "one layer");
    assert!(
        store.ref_get("@loaded/modern-latest").unwrap().is_some(),
        "the loaded ref is tagged in the store"
    );
}

/// A tag-less save (no `RepoTags`) loads under a deterministic content fallback
/// (`@loaded/img-<first12>`) and reports `tagged_from_tar = false`.
#[test]
fn load_untagged_save_uses_content_fallback() {
    let (dir, store) = tmp_store();
    let layer = make_layer(&[("f", b"data", 0o644)]);
    // make_layout writes an OCI-layout dir with NO manifest.json RepoTags; saving
    // it produces a docker-save-compat manifest.json that carries no RepoTags.
    let layout_dir = make_layout(dir.path(), &[layer]);
    import_layout(&layout_dir, &store, "src").unwrap();
    let out = dir.path().join("notag.tar");
    save("src", Some(&out), &store).unwrap();

    let report = load(Some(&out), &store).unwrap();
    assert!(!report.tagged_from_tar, "no RepoTags ⇒ content fallback");
    assert!(
        report.name.starts_with("@loaded/img-"),
        "fallback uses the @loaded/img- prefix, got {}",
        report.name
    );
    // Deterministic: loading the same tar twice yields the same fallback name.
    let report2 = load(Some(&out), &store).unwrap();
    assert_eq!(report.name, report2.name, "fallback name is deterministic");
}

/// Fail-closed: a missing input file is an honest `Io` error (exit 1), never a
/// silent empty load.
#[test]
fn load_missing_file_errors_io() {
    let (dir, store) = tmp_store();
    let missing = dir.path().join("does-not-exist.tar");
    let err = load(Some(&missing), &store).unwrap_err();
    assert!(
        matches!(err, LightrError::Io(_)),
        "missing file ⇒ Io, got {err:?}"
    );
}

/// Fail-closed: a malformed tar (random bytes, not a tar) surfaces as an honest
/// error — never a silent success.
#[test]
fn load_malformed_tar_errors() {
    let (dir, store) = tmp_store();
    let junk = dir.path().join("junk.tar");
    fs::write(&junk, b"this is definitely not a tar archive at all").unwrap();
    let err = load(Some(&junk), &store).unwrap_err();
    // Garbage either fails RepoTags scan (Io) or has no manifest.json
    // (InvalidManifest from import) — both are fail-closed, never Ok.
    assert!(
        matches!(err, LightrError::Io(_) | LightrError::InvalidManifest(_)),
        "malformed tar must fail closed, got {err:?}"
    );
}

/// `first_repo_tag` extracts the first RepoTag from a docker-save tar, and
/// returns `None` for a tar without RepoTags.
#[test]
fn first_repo_tag_extraction() {
    let layer = make_layer(&[("f", b"x", 0o644)]);
    let with_tag = make_modern_docker_save(&layer, false);
    assert_eq!(
        first_repo_tag(&with_tag).unwrap().as_deref(),
        Some("modern:latest"),
        "RepoTags[0] extracted"
    );
}

/// The sanitizer maps docker repo:tag → a valid `@loaded/` lightr ref name,
/// matching the docker-shim `/`,`:`→`-` convention.
#[test]
fn sanitize_ref_matches_docker_convention() {
    assert_eq!(sanitize_ref("nginx:1.25"), "@loaded/nginx-1.25");
    assert_eq!(
        sanitize_ref("ghcr.io/owner/repo:tag"),
        "@loaded/ghcr.io-owner-repo-tag"
    );
    // Uppercase is lowercased; out-of-grammar bytes are dropped.
    assert_eq!(sanitize_ref("MyImage:Latest"), "@loaded/myimage-latest");
    // A degenerate tag with no usable chars falls back to "untagged".
    assert_eq!(sanitize_ref("@@@"), "@loaded/untagged");
}
