//! Tests for the `lightr commit` handler — split via `#[path]` (godfile cap).
//!
//! Parallel-safe: the end-to-end `run` resolves a container via LIGHTR_HOME
//! (process-global), so those paths are exercised under the crate ENV_LOCK with
//! their own tempdir. The deterministic name helper + the snapshot-of-rootfs
//! mechanism (which `commit` composes) are tested with an injected store, no env.

use super::*;
use lightr_store::Store;

// ── generated_name (ADR-0004-valid, stable per container) ─────────────────────

#[test]
fn generated_name_is_prefixed_and_safe() {
    let n = generated_name("1717600000000000000-42");
    assert!(n.starts_with("commit-"));
    // Must be a valid ref name (ADR-0004 grammar).
    lightr_core::validate_ref_name(&n).expect("generated name must be a valid ref");
}

#[test]
fn generated_name_is_stable() {
    let id = "abc123-7";
    assert_eq!(
        generated_name(id),
        generated_name(id),
        "stable per container"
    );
}

#[test]
fn generated_name_strips_unsafe_chars_and_caps_len() {
    // Uppercase + '/' are not ADR-0004-safe; they must be filtered out.
    let n = generated_name(&format!("AB/{}", "z".repeat(80)));
    lightr_core::validate_ref_name(&n).expect("filtered name must validate");
    assert!(!n.contains('/') && !n.chars().any(|c| c.is_ascii_uppercase()));
}

// ── snapshot-of-rootfs ⇒ new ref (the mechanism commit composes) ──────────────

#[test]
fn snapshot_of_rootfs_creates_a_named_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();

    // Emulate a container rootfs dir with one file, then snapshot it under a ref
    // (this is exactly what `commit` does after resolving the container).
    let rootfs = tmp.path().join("run").join("cid").join("rootfs");
    std::fs::create_dir_all(&rootfs).unwrap();
    std::fs::write(rootfs.join("hello.txt"), b"committed contents").unwrap();

    // `lightr_index::snapshot` keys its index cache off LIGHTR_HOME (process-
    // global), so point it at this tempdir under the crate ENV_LOCK to stay
    // parallel-safe (no race on the shared index dir, no touch of real $HOME).
    let _g = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    // SAFETY: single-threaded under ENV_LOCK.
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let report = lightr_index::snapshot(&rootfs, &store, "committed-img");
    unsafe { std::env::remove_var("LIGHTR_HOME") };
    let report = report.unwrap();
    assert!(report.files >= 1, "the rootfs file is ingested");

    // The ref now resolves to the snapshot's manifest root.
    let rec = store
        .ref_get("committed-img")
        .unwrap()
        .expect("ref written");
    assert_eq!(rec.root, report.root);

    // And it appears in the image listing.
    let rows = lightr_oci::list_images(&store).unwrap();
    assert!(rows.iter().any(|r| r.repository == "committed-img"));
}

// ── end-to-end: missing container ⇒ exit 1 (Docker "No such container") ────────

#[test]
fn commit_missing_container_is_exit_1() {
    let tmp = tempfile::tempdir().unwrap();
    // Empty home ⇒ no runs ⇒ resolve miss ⇒ die_resolve ⇒ exit 1.
    let _g = crate::test_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    // SAFETY: single-threaded under ENV_LOCK.
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = run("ghost-container", None);
    unsafe { std::env::remove_var("LIGHTR_HOME") };
    assert_eq!(code, 1, "missing container ⇒ No such container, exit 1");
}
