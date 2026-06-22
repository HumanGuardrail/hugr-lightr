//! WP-C unit tests for `FROM --platform` resolution + validation.

use super::*;
use lightr_store::{ImageManifestRecord, Store};

/// A throwaway store rooted in a fresh temp dir.
fn tmp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    (dir, store)
}

#[test]
fn host_platform_is_linux_os_arch() {
    let p = host_platform();
    assert!(p.starts_with("linux/"), "host platform os is linux: {p}");
    // The arch token is one of the OCI tokens we map to (never the raw rustc one).
    assert!(
        !p.contains("x86_64") && !p.contains("aarch64"),
        "arch is OCI-mapped: {p}"
    );
}

#[test]
fn resolve_defaults_to_host_when_absent() {
    assert_eq!(resolve_platform(None), host_platform());
}

#[test]
fn resolve_normalizes_a_bare_arch_to_linux() {
    assert_eq!(resolve_platform(Some("amd64")), "linux/amd64");
    assert_eq!(resolve_platform(Some("LINUX/ARM64")), "linux/arm64");
    assert_eq!(
        resolve_platform(Some("linux/arm/v7")),
        "linux/arm/v7",
        "variant preserved"
    );
}

#[test]
fn validate_none_request_always_passes() {
    let (_d, store) = tmp_store();
    // No base record at all + no request ⇒ trivially OK.
    assert!(validate_against_base(&store, "nonexistent", None).is_ok());
}

#[test]
fn validate_scratch_always_passes() {
    let (_d, store) = tmp_store();
    assert!(validate_against_base(&store, "scratch", Some("linux/amd64")).is_ok());
}

#[test]
fn validate_no_base_record_passes() {
    let (_d, store) = tmp_store();
    // A base with no manifest record can't contradict the request (single-arch
    // import that we accept as the host materialization).
    assert!(validate_against_base(&store, "no-such-base", Some("linux/amd64")).is_ok());
}

#[test]
fn validate_matching_platform_passes() {
    let (_d, store) = tmp_store();
    store
        .image_manifest_put(
            "base-amd64",
            &ImageManifestRecord {
                manifest_bytes: b"{}".to_vec(),
                descriptors: Vec::new(),
                platform: "linux/amd64".to_string(),
            },
        )
        .unwrap();
    assert!(validate_against_base(&store, "base-amd64", Some("linux/amd64")).is_ok());
    // Case + bare-arch normalization still match.
    assert!(validate_against_base(&store, "base-amd64", Some("amd64")).is_ok());
    assert!(validate_against_base(&store, "base-amd64", Some("LINUX/AMD64")).is_ok());
}

#[test]
fn validate_mismatching_platform_errors() {
    let (_d, store) = tmp_store();
    store
        .image_manifest_put(
            "base-amd64",
            &ImageManifestRecord {
                manifest_bytes: b"{}".to_vec(),
                descriptors: Vec::new(),
                platform: "linux/amd64".to_string(),
            },
        )
        .unwrap();
    let err = validate_against_base(&store, "base-amd64", Some("linux/arm64"));
    assert!(
        err.is_err(),
        "arm64 request vs amd64 base must error honestly"
    );
    let msg = format!("{}", err.unwrap_err());
    assert!(
        msg.contains("single-arch"),
        "honest single-arch message: {msg}"
    );
}

#[test]
fn validate_variant_compatible_when_one_side_omits_it() {
    let (_d, store) = tmp_store();
    store
        .image_manifest_put(
            "base-arm",
            &ImageManifestRecord {
                manifest_bytes: b"{}".to_vec(),
                descriptors: Vec::new(),
                platform: "linux/arm/v7".to_string(),
            },
        )
        .unwrap();
    // os/arch match, request omits the variant ⇒ compatible.
    assert!(validate_against_base(&store, "base-arm", Some("linux/arm")).is_ok());
    // os/arch differ ⇒ still an error even though both have a variant slot.
    assert!(validate_against_base(&store, "base-arm", Some("linux/amd64")).is_err());
}
