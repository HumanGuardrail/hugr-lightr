//! `oci rmi <ref>...` — remove image ref(s), Docker-faithful (WP-IMG-07).
//!
//! Removing an image ref **untags** it: the named store ref and its image
//! sidecars (config + manifest record) are deleted, so the image vanishes from
//! `oci images`. The underlying CAS objects are NOT deleted here — they become
//! gc candidates, reclaimed by `lightr gc` (rmi never sweeps the CAS).
//!
//! Guards (Docker parity):
//!   • **in-use** — a ref booted by a running container (its `rootfs_ref`) is
//!     refused without `-f` ("image is being used"); `-f` force-removes it.
//!   • **absent** — an unknown ref is an error ("No such image").
//!
//! The in-use set is INJECTED (the set of `rootfs_ref`s of running instances),
//! computed by the CLI from `lightr_run::ps` — so this logic stays free of a
//! lightr-run dependency and is parallel-testable against a tempdir store.

use lightr_core::{LightrError, Result};
use lightr_store::Store;

/// What `rmi` did to one ref. Maps to Docker's `Untagged: <ref>` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RmiReport {
    /// The ref that was removed.
    pub name: String,
    /// True iff the ref was in use and removed only because `-f` was passed.
    pub forced: bool,
}

/// Remove ONE image ref. Fail-closed:
///   • absent ref ⇒ `RefNotFound` (Docker "No such image", exit 2),
///   • in-use ref without `force` ⇒ `Io(Other)` refusal (Docker "image is being
///     used", exit 1) — no untag performed,
///   • in-use ref WITH `force` ⇒ untagged anyway (`forced = true`).
///
/// `in_use` is the set of rootfs refs of currently-running instances. On
/// success the ref file, its name record, and both image sidecars are removed;
/// the CAS blobs are left as gc candidates.
pub fn rmi_one(store: &Store, name: &str, in_use: &[String], force: bool) -> Result<RmiReport> {
    // Absent ⇒ "No such image" (fail-closed, exit 2 via RefNotFound).
    if store.ref_get(name)?.is_none() {
        return Err(LightrError::RefNotFound(name.to_string()));
    }

    let busy = in_use.iter().any(|r| r == name);
    if busy && !force {
        // Docker: refusing to remove an image used by a running container is a
        // CONFLICT (exit 1), not a usage error — surface as Io(Other) so
        // die_lightr maps it to 1. No untag is performed.
        return Err(LightrError::Io(std::io::Error::other(format!(
            "conflict: unable to remove {name}: image is being used by a running container (use -f to force)"
        ))));
    }

    // Untag: drop the ref + name record, then the image sidecars. The CAS
    // objects are intentionally left in place (gc candidates).
    store.ref_remove(name)?;
    store.remove_image_sidecars(name)?;

    Ok(RmiReport {
        name: name.to_string(),
        forced: busy,
    })
}

/// One processed target's outcome, for continue-on-error multi-ref reporting.
pub enum RmiResult {
    /// The ref was removed (carries the per-ref report).
    Removed(RmiReport),
    /// The ref failed (carries the error); processing continues with the rest.
    Failed { name: String, error: LightrError },
}

/// Remove MANY refs, Docker-faithful continue-on-error: every target is
/// processed, a failure on one does not abort the rest. Returns one
/// [`RmiResult`] per input target, in order. The caller renders the per-ref
/// lines and derives the process exit code from the worst failure.
pub fn rmi_many(store: &Store, names: &[String], in_use: &[String], force: bool) -> Vec<RmiResult> {
    names
        .iter()
        .map(|name| match rmi_one(store, name, in_use, force) {
            Ok(report) => RmiResult::Removed(report),
            Err(error) => RmiResult::Failed {
                name: name.clone(),
                error,
            },
        })
        .collect()
}

/// Render the multi-ref outcome Docker-faithfully and return the process exit
/// code, keeping the CLI arm thin. Per ref: `Untagged: <ref>` (or
/// `Untagged (forced): <ref>` when `-f` overrode an in-use guard) on stdout;
/// `Error: <msg>` on stderr for each failure. Exit code is the WORST outcome:
///   • any `RefNotFound` (No such image) ⇒ 2 (usage),
///   • else any other failure ⇒ 1 (runtime),
///   • all removed ⇒ 0.
pub fn render_rmi_results(results: &[RmiResult]) -> i32 {
    let mut code = 0;
    for r in results {
        match r {
            RmiResult::Removed(rep) => {
                if rep.forced {
                    println!("Untagged (forced): {}", rep.name);
                } else {
                    println!("Untagged: {}", rep.name);
                }
            }
            RmiResult::Failed { name, error } => {
                eprintln!("Error: {name}: {error}");
                let this = match error {
                    LightrError::RefNotFound(_) | LightrError::InvalidRef(_) => 2,
                    _ => 1,
                };
                // RefNotFound (2) dominates a runtime 1; first non-zero wins
                // otherwise (a usage error is the more actionable signal).
                if code != 2 {
                    code = this;
                }
            }
        }
    }
    code
}

#[cfg(test)]
#[path = "rmi_tests.rs"]
mod tests;
