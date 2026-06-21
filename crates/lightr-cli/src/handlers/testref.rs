//! Shared parallel-safe test helper for the image-management verb handlers
//! (WP-IMAGE-VERBS). `#[cfg(test)]` only.
//!
//! Builds a tempdir store and writes a ref DIRECTLY via the store API — a
//! single-file manifest + blob + `RefRecord`. It deliberately does NOT use
//! `lightr_index::snapshot`, whose index cache is keyed off the process-global
//! `LIGHTR_HOME`/`HOME` env and therefore races (and touches the real `$HOME`)
//! under the multi-threaded test runner. This helper touches no global state, so
//! every test using it is parallel-safe with its own unique tempdir.

use lightr_core::{Digest, Entry, Manifest, RefRecord};
use lightr_store::Store;

/// Open a fresh tempdir store and write a ref named `name` whose single-file
/// manifest references `body`. Returns the tempdir (kept alive for the store's
/// lifetime) and the open store.
pub fn store_with_ref(name: &str, body: &[u8]) -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    write_ref(&store, name, body);
    (tmp, store)
}

/// Write a ref named `name` into an already-open `store` (used to add a second
/// ref to a store that already holds one). Ingests `body` as a CAS blob, builds
/// a one-file manifest, stores it, and writes the `RefRecord`.
pub fn write_ref(store: &Store, name: &str, body: &[u8]) {
    let blob = store.put_bytes(body).unwrap();
    let manifest = Manifest {
        version: 1,
        total_size: body.len() as u64,
        entries: vec![Entry::File {
            path: "f.txt".to_string(),
            mode: 0o644,
            size: body.len() as u64,
            digest: blob,
        }],
    };
    let root: Digest = manifest.digest();
    store.put_bytes(&manifest.encode()).unwrap();
    let prev = store.ref_get(name).unwrap();
    let rec = RefRecord {
        name: name.to_string(),
        root,
        parent: prev.map(|r| r.root),
        created_at_unix: 1_623_456_000,
        tool_version: "test".to_string(),
    };
    store.ref_put(&rec).unwrap();
}
