//! Push, build_layer_tar_gz, registry_scheme, upload_put_url, parse_image_ref tests.

use super::{tmp_store_and_home, ENV_LOCK};
use crate::oci::http::registry_scheme;
use crate::oci::push::{build_layer_tar_gz, push, upload_put_url};
use crate::oci::reference::parse_image_ref;
use crate::oci::util::sha256_hex_of;
use flate2::read::GzDecoder;
use lightr_core::{LightrError, Manifest};
use std::{fs, io::Read};
use tempfile::TempDir;

// ── parse_image_ref unit tests ────────────────────────────────────────────

#[test]
fn test_parse_image_ref_simple_name() {
    let (reg, repo, tag) = parse_image_ref("alpine").unwrap();
    assert_eq!(reg, "registry-1.docker.io");
    assert_eq!(repo, "library/alpine");
    assert_eq!(tag, "latest");
}

#[test]
fn test_parse_image_ref_with_tag() {
    let (reg, repo, tag) = parse_image_ref("ubuntu:22.04").unwrap();
    assert_eq!(reg, "registry-1.docker.io");
    assert_eq!(repo, "library/ubuntu");
    assert_eq!(tag, "22.04");
}

#[test]
fn test_parse_image_ref_namespaced() {
    let (reg, repo, tag) = parse_image_ref("myorg/myimage:v1").unwrap();
    assert_eq!(reg, "registry-1.docker.io");
    assert_eq!(repo, "myorg/myimage");
    assert_eq!(tag, "v1");
}

#[test]
fn test_parse_image_ref_custom_registry() {
    let (reg, repo, tag) = parse_image_ref("ghcr.io/owner/repo:sha256abc").unwrap();
    assert_eq!(reg, "ghcr.io");
    assert_eq!(repo, "owner/repo");
    assert_eq!(tag, "sha256abc");
}

#[test]
fn test_parse_image_ref_default_tag() {
    let (reg, repo, tag) = parse_image_ref("nginx").unwrap();
    assert_eq!(reg, "registry-1.docker.io");
    assert_eq!(repo, "library/nginx");
    assert_eq!(tag, "latest");
}

/// FIX 6: empty ref → InvalidRef
#[test]
fn test_parse_image_ref_empty_is_invalid() {
    assert!(matches!(
        parse_image_ref(""),
        Err(LightrError::InvalidRef(_))
    ));
    assert!(matches!(
        parse_image_ref("   "),
        Err(LightrError::InvalidRef(_))
    ));
}

/// FIX 6: bad chars in repo → InvalidRef
#[test]
fn test_parse_image_ref_bad_chars_invalid() {
    // space in name
    assert!(matches!(
        parse_image_ref("my repo:tag"),
        Err(LightrError::InvalidRef(_))
    ));
    // shell metachar
    assert!(matches!(
        parse_image_ref("foo;bar"),
        Err(LightrError::InvalidRef(_))
    ));
}

// ── WP-PUSH: registry_scheme ──────────────────────────────────────────────

#[test]
fn test_registry_scheme_localhost_is_http() {
    assert_eq!(registry_scheme("localhost"), "http://");
    assert_eq!(registry_scheme("localhost:5000"), "http://");
    assert_eq!(registry_scheme("127.0.0.1"), "http://");
    assert_eq!(registry_scheme("127.0.0.1:5000"), "http://");
}

#[test]
fn test_registry_scheme_remote_is_https() {
    assert_eq!(registry_scheme("registry-1.docker.io"), "https://");
    assert_eq!(registry_scheme("ghcr.io"), "https://");
    assert_eq!(registry_scheme("myregistry.example.com:5000"), "https://");
}

// ── WP-PUSH: upload_put_url ────────────────────────────────────────────────

#[test]
fn test_upload_put_url_appends_digest() {
    // Absolute Location with existing query → use '&'.
    let u = upload_put_url(
        "https://",
        "ghcr.io",
        "https://ghcr.io/v2/o/r/blobs/uploads/abc?state=xyz",
        "deadbeef",
    );
    assert_eq!(
        u,
        "https://ghcr.io/v2/o/r/blobs/uploads/abc?state=xyz&digest=sha256:deadbeef"
    );

    // Absolute Location with no query → use '?'.
    let u = upload_put_url(
        "https://",
        "ghcr.io",
        "https://ghcr.io/v2/o/r/blobs/uploads/abc",
        "deadbeef",
    );
    assert_eq!(
        u,
        "https://ghcr.io/v2/o/r/blobs/uploads/abc?digest=sha256:deadbeef"
    );

    // Registry-relative Location (leading slash) → prefix scheme+registry.
    let u = upload_put_url(
        "http://",
        "localhost:5000",
        "/v2/o/r/blobs/uploads/abc?x=1",
        "cafe",
    );
    assert_eq!(
        u,
        "http://localhost:5000/v2/o/r/blobs/uploads/abc?x=1&digest=sha256:cafe"
    );
}

// ── WP-PUSH: build_layer_tar_gz — well-formed + stable digests ─────────────

/// Synthesize a layer from a hydrated tree and assert that the layer digest,
/// diff_id, and size are well-formed (64-hex) and STABLE across two runs of
/// the same tree (gzip is deterministic at a fixed compression level here).
#[test]
fn test_build_layer_tar_gz_stable_and_wellformed() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = TempDir::new().unwrap();
    let (_home, store) = tmp_store_and_home();

    // Snapshot a small fixture tree into the store, then read its Manifest.
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("etc")).unwrap();
    fs::write(src.join("etc/conf"), b"k=v\n").unwrap();
    fs::write(src.join("hello"), b"hi").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(src.join("hello"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let snap = lightr_index::snapshot(&src, &store, "@t/push-fix").unwrap();
    let manifest_bytes = store.get_bytes(&snap.root).unwrap();
    let tree = Manifest::decode(&manifest_bytes).unwrap();

    let p1 = tmp.path().join("l1.tar.gz");
    let p2 = tmp.path().join("l2.tar.gz");
    let (layer1, diff1, size1) = build_layer_tar_gz(&tree, &store, &p1).unwrap();
    let (layer2, diff2, size2) = build_layer_tar_gz(&tree, &store, &p2).unwrap();

    // Well-formed: 64-char lowercase hex.
    for h in [&layer1, &diff1] {
        assert_eq!(h.len(), 64, "digest must be 64 hex chars: {h}");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "digest must be lowercase hex: {h}"
        );
    }
    // diff_id (uncompressed) must differ from the gzipped layer digest.
    assert_ne!(layer1, diff1, "layer digest must differ from diff_id");
    // Stable across runs of the same tree.
    assert_eq!(layer1, layer2, "layer digest must be stable");
    assert_eq!(diff1, diff2, "diff_id must be stable");
    assert_eq!(size1, size2, "gzipped size must be stable");

    // The on-disk gzipped file size matches the reported size, and the
    // digest is the real sha256 of those bytes.
    let on_disk = fs::read(&p1).unwrap();
    assert_eq!(on_disk.len() as u64, size1, "reported size matches file");
    assert_eq!(
        sha256_hex_of(&on_disk),
        layer1,
        "layer digest must be sha256 of the gzipped bytes"
    );

    // The diff_id must equal the sha256 of the UNCOMPRESSED tar.
    let mut gz = GzDecoder::new(&on_disk[..]);
    let mut raw = Vec::new();
    gz.read_to_end(&mut raw).unwrap();
    assert_eq!(
        sha256_hex_of(&raw),
        diff1,
        "diff_id must be sha256 of the uncompressed tar"
    );
}

/// push of an unknown ref → RefNotFound (fail-closed, no network touched).
#[test]
fn test_push_unknown_ref_fails_closed() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (_home, store) = tmp_store_and_home();
    let err = push("@t/does-not-exist", "localhost:5000/x:latest", &store).unwrap_err();
    assert!(
        matches!(err, LightrError::RefNotFound(_)),
        "unknown ref must be RefNotFound; got: {err:?}"
    );
}
