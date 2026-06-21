//! `lightr commit <container> [<ref>]` handler — the top-level `docker commit`
//! verb mapped onto the lightr ref registry (WP-IMAGE-VERBS).
//!
//! LEAD DESIGN (frozen): a Docker "image" = a named lightr ref. `commit` freezes
//! a container's current filesystem into a new image: resolve `<container>` (a
//! detached run — the same resolution `cp` uses), snapshot its rootfs dir via
//! the existing `lightr_index::snapshot` machinery (CAS-ingests new chunks +
//! writes a manifest), and tag the result under `<ref>` (or a generated content
//! name when omitted). Docker prints the new image id on commit; we print the
//! manifest root digest as `sha256:<id>` (lightr digests are not sha256, but the
//! Docker shape is preserved — the hex IS the image id).
//!
//! A "container" here is a detached run; its materialized filesystem root is
//! `<home>/run/<id>/rootfs` (created on spawn — see `lightr_run::run::svz`),
//! resolved with `lightr_run::resolve`. A miss routes through `die_resolve` ⇒
//! "No such container" + exit **1** (Docker parity).
//!
//! Exit codes: missing container ⇒ 1 (parity). Invalid `<ref>` name ⇒ 2
//! (usage). Snapshot/store fault ⇒ 1. An absent rootfs (e.g. a `native` run with
//! no separate filesystem) ⇒ honest exit 1 (never a silent empty image).
//!
//! Memo: registry op only — touches no build/run memo keys.

use lightr_core::validate_ref_name;
use lightr_index::snapshot;
use lightr_store::Store;

use crate::{exit::die_lightr, exit::die_resolve, lightr_home};

/// `lightr commit <container> [<ref>]`.
pub fn run(container: &str, reference: Option<&str>) -> i32 {
    // Validate an explicit ref name up-front: a bad name is a usage error
    // (exit 2). An omitted ref is generated post-snapshot from the content.
    if let Some(name) = reference {
        if let Err(e) = validate_ref_name(name) {
            return die_lightr(&e); // InvalidRef ⇒ exit 2
        }
    }

    let home = lightr_home();

    // Resolve the container (detached run) → its rootfs dir. Miss ⇒ "No such
    // container" + exit 1 (Docker parity, same path as `cp`).
    let id = match lightr_run::resolve(&home, container) {
        Ok(id) => id,
        Err(e) => return die_resolve(&e, container),
    };
    let rootfs = home.join("run").join(&id).join("rootfs");
    if !rootfs.is_dir() {
        // A run with no separate filesystem (e.g. native) has nothing to commit.
        // Fail-closed: honest error, never a silent empty image.
        eprintln!(
            "Error: container {container} has no committable filesystem (no rootfs at {})",
            rootfs.display()
        );
        return 1;
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    // A ref name MUST be known before snapshot (snapshot writes the ref). When
    // omitted, pre-compute a deterministic content name by first snapshotting to
    // a temporary holding ref is wasteful; instead snapshot to the explicit name,
    // or to a generated name derived from the container id when omitted (still
    // unique per container, ADR-0004-valid, and stable across re-commits of the
    // same container).
    let name = match reference {
        Some(n) => n.to_string(),
        None => generated_name(&id),
    };

    let report = match snapshot(&rootfs, &store, &name) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    // Docker prints the new image id. Preserve the shape (`sha256:` prefix); the
    // hex is the manifest root digest. The ref name follows so the operator can
    // address the image by name too.
    println!("sha256:{} (tagged {name})", report.root.to_hex());
    0
}

/// Generate an ADR-0004-valid ref name for an omitted-`<ref>` commit. Derived
/// from the (already-validated, lowercase-hex/alnum) container id so re-commits
/// of the same container are stable; prefixed `commit-` to namespace it.
fn generated_name(id: &str) -> String {
    // The container id is a run id (alphanumeric); keep only ADR-0004-safe
    // chars and cap the local-part length (<=64) with the prefix included.
    let safe: String = id
        .chars()
        .filter(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '.' || *c == '_' || *c == '-'
        })
        .take(50)
        .collect();
    format!("commit-{safe}")
}

#[cfg(test)]
#[path = "commit_tests.rs"]
mod tests;
