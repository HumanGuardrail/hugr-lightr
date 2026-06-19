//! View backend seam, fake test double, solidifier policy, and one-step driver.

use std::path::Path;

use super::plan::{EntryKind, PlanEntry, ViewPlan};

// ── ViewBackend ──────────────────────────────────────────────────────────────

/// OS actions a view backend performs — seamed for host testing.
///
/// Planned O(1) impls per ADR-0013: composefs/EROFS (Linux), NFS-loopback
/// (macOS, EdenFS-proven), ProjFS (Windows). Those are `cfg`-gated modules
/// that are **intentionally not yet wired into the run path** — wiring them
/// in is the ADR-0013 S1/S3 spike work. The host-tested double is
/// [`FakeBackend`].
pub trait ViewBackend {
    /// Mount the view described by `plan` at `at` (O(1) appearance).
    fn mount(&mut self, plan: &ViewPlan, at: &Path) -> std::io::Result<()>;
    /// Lazily load one entry's content into the live view.
    fn fault_in(&mut self, path: &str) -> std::io::Result<()>;
    /// Tear the view down (after full solidification it can evaporate).
    fn unmount(&mut self, at: &Path) -> std::io::Result<()>;
}

/// A host-test double: records every `mount` / `fault_in` / `unmount` call and
/// the set of faulted-in paths, so tests can assert backend interactions
/// without a real kernel mount. `fault_in` is idempotent on the faulted set.
#[derive(Debug, Default)]
pub struct FakeBackend {
    /// Paths passed to `mount`, in call order.
    pub mounted: Vec<std::path::PathBuf>,
    /// Paths passed to `unmount`, in call order.
    pub unmounted: Vec<std::path::PathBuf>,
    /// Every `fault_in` call's path, in call order (may repeat).
    pub fault_calls: Vec<String>,
    /// Distinct set of paths that have been faulted in.
    pub faulted: std::collections::BTreeSet<String>,
}

impl FakeBackend {
    /// A fresh recorder with nothing mounted or faulted.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ViewBackend for FakeBackend {
    fn mount(&mut self, _plan: &ViewPlan, at: &Path) -> std::io::Result<()> {
        self.mounted.push(at.to_path_buf());
        Ok(())
    }

    fn fault_in(&mut self, path: &str) -> std::io::Result<()> {
        self.fault_calls.push(path.to_string());
        self.faulted.insert(path.to_string());
        Ok(())
    }

    fn unmount(&mut self, at: &Path) -> std::io::Result<()> {
        self.unmounted.push(at.to_path_buf());
        Ok(())
    }
}

// ── Solidifier ───────────────────────────────────────────────────────────────

/// Per-file solidification bookkeeping inside the [`Solidifier`].
#[derive(Clone, Debug)]
pub(crate) struct FileState {
    /// Path of the file (matches the plan entry).
    path: String,
    /// `true` once `record_access` has been called for this path — it's on
    /// the critical path, so it's promoted first.
    accessed: bool,
    /// `true` once `next_to_promote` has handed this entry out (in flight).
    handed_out: bool,
    /// `true` once the caller has confirmed the CoW clone via `mark_promoted`.
    confirmed: bool,
}

/// Promote-on-access policy: decides which entries to CoW-clone to real disk
/// and when the mount can evaporate. **Pure + fully unit-tested.**
///
/// # Seeding
///
/// [`Solidifier::new`] seeds the set of all **promotable** entries — files
/// only. Directories and symlinks are cheap metadata the backend creates at
/// mount time, so they are never promotion candidates and never affect
/// [`Solidifier::is_fully_solid`].
///
/// # Policy (pre-decided, documented, deterministic)
///
/// **Promote accessed entries first (they're on the critical path), then the
/// rest in manifest order so the mount can fully evaporate.** Concretely,
/// [`Solidifier::next_to_promote`] returns the highest-priority entry that has
/// not yet been handed out:
///
/// 1. **Hot before cold** — entries that have been `record_access`-ed come
///    before entries that have not.
/// 2. **Manifest order as the stable tiebreak** — within each band, the
///    plan's path-sorted order decides, so the sequence is fully deterministic
///    regardless of access timing.
///
/// `next_to_promote` marks the returned entry *handed out* (it won't be
/// returned again); the caller confirms the actual CoW clone landed by calling
/// [`Solidifier::mark_promoted`]. It returns `None` once every file has been
/// handed out — there is nothing left to promote.
///
/// # Fully solid
///
/// [`Solidifier::is_fully_solid`] is `true` only once **every** file entry has
/// been *confirmed* (handed out by `next_to_promote` **and** confirmed by
/// `mark_promoted`). At that point the mount can evaporate: steady state is
/// native, zero indirection. A view with no files is trivially fully solid.
pub struct Solidifier {
    /// File states in plan (path-sorted / manifest) order — the priority
    /// tiebreak walks this in order.
    pub(crate) files: Vec<FileState>,
}

impl Solidifier {
    /// Seed the solidifier from a plan: every file entry becomes a promotion
    /// candidate (dirs/symlinks are cheap, created at mount, never promoted).
    pub fn new(plan: &ViewPlan) -> Self {
        let files = plan
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| FileState {
                path: e.path.clone(),
                accessed: false,
                handed_out: false,
                confirmed: false,
            })
            .collect();
        Self { files }
    }

    /// Mark an entry hot (on the critical path). Idempotent: recording the
    /// same path again is a no-op. A path that isn't a promotable file (a dir,
    /// a symlink, or simply unknown) is ignored — only files are promoted.
    pub fn record_access(&mut self, path: &str) {
        if let Some(f) = self.files.iter_mut().find(|f| f.path == path) {
            f.accessed = true;
        }
    }

    /// Return the highest-priority not-yet-handed-out entry per the documented
    /// policy (hot before cold; manifest order as the stable tiebreak), and
    /// mark it handed out. Returns `None` when every file has been handed out.
    ///
    /// The returned path is *in flight*; the caller confirms the CoW clone via
    /// [`Solidifier::mark_promoted`].
    pub fn next_to_promote(&mut self) -> Option<String> {
        // Band 1: hot + not-yet-handed-out, first in manifest order.
        // Band 2: cold + not-yet-handed-out, first in manifest order.
        // `files` is already in manifest order, so the first match in each
        // pass is the stable-tiebreak winner.
        let idx = self
            .files
            .iter()
            .position(|f| f.accessed && !f.handed_out)
            .or_else(|| self.files.iter().position(|f| !f.handed_out));

        idx.map(|i| {
            self.files[i].handed_out = true;
            self.files[i].path.clone()
        })
    }

    /// Confirm that the CoW clone for `path` has landed (the caller drove the
    /// store). Confirming a path that isn't a promotable file is ignored.
    pub fn mark_promoted(&mut self, path: &str) {
        if let Some(f) = self.files.iter_mut().find(|f| f.path == path) {
            f.confirmed = true;
        }
    }

    /// `true` once **every** file entry has been confirmed promoted — at which
    /// point the mount can evaporate. A view with no files is trivially solid.
    pub fn is_fully_solid(&self) -> bool {
        self.files.iter().all(|f| f.confirmed)
    }
}

// ── Optional driver ──────────────────────────────────────────────────────────

/// Drive **one** solidification step against a [`ViewBackend`] and the store:
/// ask the solidifier for the next entry to promote, drive its CoW clone via
/// the store into `dest_root`, fault it into the live view, and mark it
/// promoted. Returns the promoted path, or `None` when nothing is left.
///
/// This is the pure-enough driver: it's host-testable with [`FakeBackend`]
/// and a real [`lightr_store::Store`]. It is deliberately **not** wired into
/// `hydrate`/CLI — that is an additive follow-up, out of scope for this WP.
pub fn solidify_step(
    plan: &ViewPlan,
    backend: &mut dyn ViewBackend,
    solidifier: &mut Solidifier,
    store: &lightr_store::Store,
    dest_root: &Path,
) -> std::io::Result<Option<String>> {
    let Some(path) = solidifier.next_to_promote() else {
        return Ok(None);
    };

    // Find the plan entry for the digest/mode. Only files are promotable, so
    // a handed-out path always resolves to a File entry with a digest.
    let entry = plan
        .entries
        .iter()
        .find(|e| e.path == path)
        .filter(|e| e.kind == EntryKind::File);

    if let Some(PlanEntry {
        digest: Some(digest),
        ..
    }) = entry
    {
        // CoW-clone the content out of the store onto real disk. The store
        // owns the rung (clonefile / reflink / copy fallback). Mode 0o644 is
        // a reasonable default for a promoted file; the manifest's mode could
        // be threaded through the plan in a follow-up if needed.
        let dest = dest_root.join(&path);
        store
            .materialize_file(digest, &dest, 0o644)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }

    backend.fault_in(&path)?;
    solidifier.mark_promoted(&path);
    Ok(Some(path))
}
