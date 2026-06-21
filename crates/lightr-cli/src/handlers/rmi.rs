//! `lightr rmi <ref>...` handler — the top-level `docker rmi` verb mapped onto
//! the lightr ref registry (WP-IMAGE-VERBS).
//!
//! LEAD DESIGN (frozen): a Docker "image" = a named lightr ref. `rmi` UNTAGS the
//! ref: it drops the named ref record (and its image sidecars), so the image
//! vanishes from `lightr images`. The underlying CAS chunks are NOT deleted
//! here — they become gc candidates, reclaimed by `lightr gc`. (Docker's own
//! `rmi` likewise removes the image but defers space reclamation to its prune
//! pass; chunk reclamation in lightr is GC-DEFERRED, never forced here.)
//!
//! Per Docker `docker rmi`:
//!   • each removed ref prints `Untagged: <ref>` on stdout;
//!   • a missing ref is an error `No such image: <ref>` on stderr and exits
//!     **1** (Docker's `rmi` treats a missing image as a runtime error, exit 1
//!     — NOT a usage error). Processing is continue-on-error: every target is
//!     attempted, and the worst outcome sets the exit code.
//!
//! Exit codes: missing image ⇒ 1 (parity). Store I/O fault ⇒ 1. (Arg/usage
//! errors are clap's domain, exit 2.)
//!
//! Memo: registry op only — touches no build/run memo keys.

use lightr_store::Store;

use crate::exit::die_lightr;

/// `lightr rmi <targets...>`.
pub fn run(targets: &[String]) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };
    rmi_in_store(&store, targets)
}

/// Core of `rmi`, store injected (parallel-safe — no process-global env).
/// Continue-on-error across all targets; returns the process exit code.
pub(crate) fn rmi_in_store(store: &Store, targets: &[String]) -> i32 {
    use lightr_core::LightrError;
    let mut code = 0;
    for name in targets {
        match remove_one(store, name) {
            Ok(()) => println!("Untagged: {name}"),
            Err(LightrError::RefNotFound(_)) => {
                // Docker shape: a missing image is "No such image" + exit 1.
                eprintln!("Error: No such image: {name}");
                code = 1;
            }
            Err(e) => {
                // A real store fault — surface it honestly (still exit 1; Docker
                // rmi has no exit-2 path for image removal).
                eprintln!("Error: {name}: {e}");
                code = 1;
            }
        }
    }
    code
}

/// Remove ONE ref (untag). Fail-closed:
///   • absent ref ⇒ `RefNotFound` ⇒ the caller prints "No such image" + exit 1;
///   • present ⇒ drop the ref record + image sidecars (CAS blobs left as gc
///     candidates, never swept here).
fn remove_one(store: &Store, name: &str) -> lightr_core::Result<()> {
    use lightr_core::LightrError;
    if store.ref_get(name)?.is_none() {
        return Err(LightrError::RefNotFound(name.to_string()));
    }
    store.ref_remove(name)?;
    store.remove_image_sidecars(name)?;
    Ok(())
}

#[cfg(test)]
#[path = "rmi_tests.rs"]
mod tests;
