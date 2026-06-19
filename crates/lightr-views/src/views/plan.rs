//! View plan: O(1)-appearance contract — walk a manifest into a [`ViewPlan`]
//! without touching disk.

use lightr_core::{Digest, Manifest};

// ── ViewPlan ─────────────────────────────────────────────────────────────────

/// The kind of a planned entry. Files are the only **promotable** kind —
/// directories and symlinks are cheap metadata the backend materializes at
/// mount time, so the solidifier never has to clone them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file (carries a content [`Digest`] in the plan).
    File,
    /// A symlink — cheap, created at mount, never promoted.
    Symlink,
    /// A directory — cheap, created at mount, never promoted.
    Dir,
}

/// One entry of a [`ViewPlan`]: enough to drive fault-in + solidification
/// without re-reading the manifest. Files carry their content [`Digest`]
/// (the key the store CoW-clones from); dirs/symlinks carry `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    /// Path of the entry, relative to the mount root.
    pub path: String,
    /// What kind of filesystem object this is.
    pub kind: EntryKind,
    /// Content digest — `Some` for [`EntryKind::File`], `None` otherwise.
    pub digest: Option<Digest>,
}

/// A plan to present a manifest as a virtual view — appears in O(1),
/// entries fault in lazily; the solidifier promotes hot ones to real CoW.
///
/// Building the plan is the O(1)-appearance contract: it walks the manifest
/// in memory and **does not touch disk**.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewPlan {
    /// Every entry of the manifest, path-sorted. This order is the stable
    /// tiebreak the [`Solidifier`] uses ("manifest order").
    pub(crate) entries: Vec<PlanEntry>,
}

impl ViewPlan {
    /// All planned entries, in path-sorted (manifest) order.
    pub fn entries(&self) -> &[PlanEntry] {
        &self.entries
    }

    /// Number of planned entries (all kinds).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the plan is empty (no entries at all).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Count of promotable entries (files only). This is the denominator the
    /// solidifier drives toward when deciding the mount may evaporate.
    pub fn file_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .count()
    }
}

/// Build a view plan from a manifest: collect **every** File / Symlink / Dir
/// entry (path-sorted) and record what's needed to fault-in + solidify.
///
/// This is the O(1)-appearance contract — it walks the manifest in memory and
/// never touches disk. Files carry their digest; dirs/symlinks carry `None`.
pub fn plan_view(manifest: &Manifest) -> ViewPlan {
    use lightr_core::Entry;

    let mut entries: Vec<PlanEntry> = manifest
        .entries
        .iter()
        .map(|e| match e {
            Entry::File { path, digest, .. } => PlanEntry {
                path: path.clone(),
                kind: EntryKind::File,
                digest: Some(*digest),
            },
            Entry::Symlink { path, .. } => PlanEntry {
                path: path.clone(),
                kind: EntryKind::Symlink,
                digest: None,
            },
            Entry::Dir { path } => PlanEntry {
                path: path.clone(),
                kind: EntryKind::Dir,
                digest: None,
            },
        })
        .collect();

    // The manifest is already path-sorted (LMF1 invariant), but the plan's
    // path-sorted order is a load-bearing contract (it's the solidifier's
    // stable tiebreak), so we make it true here rather than assume it.
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    ViewPlan { entries }
}
