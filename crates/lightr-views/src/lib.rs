//! lightr-views — O(1) materialization (ADR-0013), the other half of the
//! perf headline. The PLAN + SOLIDIFIER logic is pure and host-tested; real
//! mount backends (composefs/EROFS on Linux, NFS-loopback on macOS — the
//! EdenFS-proven route) are `cfg`-gated and marked `// VIEW-PATH (S1/S3)`:
//! they compile here, runtime validation is the S1/S3 spike on a target box.
//! NO unmeasured runtime claim. Bodies: WP-W5.
//!
//! # The contract (pre-decided law)
//!
//! * [`plan_view`] walks every manifest entry (path-sorted) into a
//!   [`ViewPlan`]. The plan is the **O(1)-appearance** contract: building it
//!   never touches disk — it only records enough per entry (path, kind, and
//!   the file digest) to later drive fault-in and solidification.
//! * [`Solidifier`] is the heart and is **pure + fully host-tested**. It owns
//!   the promote-on-access policy and the "is the mount allowed to evaporate
//!   yet?" question. See [`Solidifier`] for the documented policy.
//! * [`ViewBackend`] is the seam to the OS. [`FakeBackend`] is the host-test
//!   double; the real backends are `cfg`-gated skeletons (see the
//!   platform `composefs`/`nfsloopback` modules) that compile but are **not**
//!   runtime-validated on this box.

// The pure logic carries no `unsafe`. We deliberately do NOT
// `#![forbid(unsafe_code)]` at the crate root: the cfg-gated real backends
// (NFS-loopback server, EROFS/composefs layout) will need localized `unsafe`
// for the syscall/FFI boundary, and that is contained inside those modules.

use lightr_core::{Digest, Manifest};
use std::path::Path;

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
    entries: Vec<PlanEntry>,
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

// ── ViewBackend ──────────────────────────────────────────────────────────────

/// OS actions a view backend performs — seamed for host testing.
///
/// Real impls: composefs/EROFS (Linux), NFS-loopback (macOS, EdenFS-proven).
/// Those are `cfg`-gated skeletons that compile but are not runtime-validated
/// on this box; the host-tested double is [`FakeBackend`].
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
struct FileState {
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
    files: Vec<FileState>,
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

// ── Real backends (cfg-gated skeletons — VIEW-PATH (S1/S3)) ──────────────────

/// composefs / EROFS view backend for Linux (kernel-native, no FUSE).
///
/// EROFS metadata image is generated over the store's objects, mounted with an
/// overlay upper layer for writes; pairs with fs-verity (ADR-0013 §1).
///
/// **VIEW-PATH (S1/S3):** this is a compile-only skeleton. The real-syscall
/// functions are marked individually; runtime validation is the S1/S3 spike on
/// a Linux target box. NO unmeasured runtime claim.
#[cfg(target_os = "linux")]
pub mod composefs {
    use super::{ViewBackend, ViewPlan};
    use std::path::Path;

    /// composefs/EROFS-backed view. Holds whatever the kernel mount needs
    /// (loop device, EROFS image path, overlay dirs) once implemented.
    #[derive(Debug, Default)]
    pub struct ComposefsBackend {
        // Skeleton: real fields (EROFS image path, loop fd, overlay upper/work
        // dirs) land with the S1/S3 implementation.
        _seam: (),
    }

    impl ComposefsBackend {
        /// Construct an unmounted composefs backend.
        pub fn new() -> Self {
            Self::default()
        }
    }

    /// VIEW-PATH (S1/S3): build the EROFS metadata image describing `plan`
    /// over the store's objects. Real mkfs.erofs / image-layout work; not yet
    /// implemented, not runtime-validated.
    // VIEW-PATH (S1/S3)
    fn build_erofs_image(_plan: &ViewPlan, _image_out: &Path) -> std::io::Result<()> {
        // VIEW-PATH (S1/S3): emit EROFS over the store layout (no validation).
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "composefs build_erofs_image: VIEW-PATH (S1/S3) skeleton — not implemented",
        ))
    }

    impl ViewBackend for ComposefsBackend {
        // VIEW-PATH (S1/S3)
        fn mount(&mut self, plan: &ViewPlan, at: &Path) -> std::io::Result<()> {
            // VIEW-PATH (S1/S3): build EROFS image, loop-mount it, stack an
            // overlay upper for writes. Real kernel syscalls (mount(2)) —
            // localized unsafe lands here. Not runtime-validated.
            let _ = build_erofs_image(plan, at);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "composefs mount: VIEW-PATH (S1/S3) skeleton — not implemented",
            ))
        }

        // VIEW-PATH (S1/S3)
        fn fault_in(&mut self, _path: &str) -> std::io::Result<()> {
            // VIEW-PATH (S1/S3): EROFS faults pages from the store via the
            // kernel; this hook is for predictive readahead. Not validated.
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "composefs fault_in: VIEW-PATH (S1/S3) skeleton — not implemented",
            ))
        }

        // VIEW-PATH (S1/S3)
        fn unmount(&mut self, _at: &Path) -> std::io::Result<()> {
            // VIEW-PATH (S1/S3): umount(2) the overlay + loop device once the
            // solidifier reports fully-solid. Not runtime-validated.
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "composefs unmount: VIEW-PATH (S1/S3) skeleton — not implemented",
            ))
        }
    }
}

/// NFS-loopback view backend for macOS (the EdenFS-proven route).
///
/// An in-process NFSv3 server bound to loopback answers the kernel NFS client
/// mounted at the view root; reads fault content from the store (ADR-0013 §2).
/// FSKit is adopted later when S1-class testing says it's ready.
///
/// **VIEW-PATH (S1/S3):** this is a compile-only skeleton. The real
/// server/syscall functions are marked individually; runtime validation is the
/// S1/S3 spike on a macOS target box. NO unmeasured runtime claim.
#[cfg(target_os = "macos")]
pub mod nfsloopback {
    use super::{ViewBackend, ViewPlan};
    use std::path::Path;

    /// In-process NFS-loopback server skeleton. Holds the bound socket, the
    /// served plan, and the mount point once implemented.
    #[derive(Debug, Default)]
    pub struct NfsLoopbackBackend {
        // Skeleton: real fields (listening socket, export table, mount point,
        // server task handle) land with the S1/S3 implementation.
        _seam: (),
    }

    impl NfsLoopbackBackend {
        /// Construct an unmounted, unstarted NFS-loopback backend.
        pub fn new() -> Self {
            Self::default()
        }
    }

    /// VIEW-PATH (S1/S3): bind the in-process NFSv3 server to loopback and
    /// start serving `plan`. Real socket/server code; not yet implemented,
    /// not runtime-validated.
    // VIEW-PATH (S1/S3)
    fn start_nfs_server(_plan: &ViewPlan) -> std::io::Result<()> {
        // VIEW-PATH (S1/S3): bind 127.0.0.1, register the export, spawn the
        // RPC loop that faults content from the store. Not validated.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "nfsloopback start_nfs_server: VIEW-PATH (S1/S3) skeleton — not implemented",
        ))
    }

    impl ViewBackend for NfsLoopbackBackend {
        // VIEW-PATH (S1/S3)
        fn mount(&mut self, plan: &ViewPlan, at: &Path) -> std::io::Result<()> {
            // VIEW-PATH (S1/S3): start the loopback NFS server, then mount(2)
            // the kernel NFS client at `at`. Localized unsafe lands here.
            // Not runtime-validated.
            let _ = at; // future mount point for the kernel NFS client
            let _ = start_nfs_server(plan);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "nfsloopback mount: VIEW-PATH (S1/S3) skeleton — not implemented",
            ))
        }

        // VIEW-PATH (S1/S3)
        fn fault_in(&mut self, _path: &str) -> std::io::Result<()> {
            // VIEW-PATH (S1/S3): server-side readahead — pre-stage one entry's
            // content from the store so the next NFS READ is warm. Not validated.
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "nfsloopback fault_in: VIEW-PATH (S1/S3) skeleton — not implemented",
            ))
        }

        // VIEW-PATH (S1/S3)
        fn unmount(&mut self, _at: &Path) -> std::io::Result<()> {
            // VIEW-PATH (S1/S3): unmount(2) the NFS client and stop the server
            // once the solidifier reports fully-solid. Not runtime-validated.
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "nfsloopback unmount: VIEW-PATH (S1/S3) skeleton — not implemented",
            ))
        }
    }
}

// ── Tests (host, runnable — the proof of the pure O(1)-plan + solidifier) ────

#[cfg(test)]
mod tests {
    use super::*;
    use lightr_core::{Digest, Entry, Manifest};

    /// A digest with a recognizable single-byte fill, for stable assertions.
    fn digest(fill: u8) -> Digest {
        Digest([fill; 32])
    }

    /// A multi-kind manifest, intentionally given OUT of path-sorted order so
    /// `plan_view` is exercised on its sort guarantee.
    fn multi_kind_manifest() -> Manifest {
        let entries = vec![
            Entry::Dir { path: "src".into() },
            Entry::File {
                path: "src/main.rs".into(),
                mode: 0o644,
                size: 10,
                digest: digest(1),
            },
            Entry::Symlink {
                path: "link".into(),
                target: "src/main.rs".into(),
            },
            Entry::File {
                path: "Cargo.toml".into(),
                mode: 0o644,
                size: 20,
                digest: digest(2),
            },
            Entry::File {
                path: "src/lib.rs".into(),
                mode: 0o644,
                size: 30,
                digest: digest(3),
            },
        ];
        Manifest {
            version: 1,
            total_size: 60,
            entries,
        }
    }

    #[test]
    fn plan_view_covers_every_entry_path_sorted() {
        let m = multi_kind_manifest();
        let plan = plan_view(&m);

        // Every entry appears, exactly once.
        assert_eq!(plan.len(), m.entries.len());
        let got: Vec<&str> = plan.entries().iter().map(|e| e.path.as_str()).collect();
        // Path-sorted, regardless of the manifest's input order.
        assert_eq!(
            got,
            vec!["Cargo.toml", "link", "src", "src/lib.rs", "src/main.rs"]
        );

        // Kinds are preserved and files carry their digest; others carry None.
        let by_path = |p: &str| plan.entries().iter().find(|e| e.path == p).unwrap();
        assert_eq!(by_path("Cargo.toml").kind, EntryKind::File);
        assert_eq!(by_path("Cargo.toml").digest, Some(digest(2)));
        assert_eq!(by_path("src").kind, EntryKind::Dir);
        assert_eq!(by_path("src").digest, None);
        assert_eq!(by_path("link").kind, EntryKind::Symlink);
        assert_eq!(by_path("link").digest, None);
        assert_eq!(by_path("src/main.rs").digest, Some(digest(1)));

        // Only the 3 files are promotable.
        assert_eq!(plan.file_count(), 3);
    }

    #[test]
    fn plan_view_handles_empty_manifest() {
        let plan = plan_view(&Manifest {
            version: 1,
            total_size: 0,
            entries: vec![],
        });
        assert!(plan.is_empty());
        assert_eq!(plan.file_count(), 0);
    }

    #[test]
    fn solidifier_seeds_only_files() {
        let plan = plan_view(&multi_kind_manifest());
        let s = Solidifier::new(&plan);
        // 3 files seeded; dirs/symlinks are not promotion candidates.
        assert_eq!(s.files.len(), 3);
        // Nothing promoted yet, so not fully solid.
        assert!(!s.is_fully_solid());
    }

    #[test]
    fn solidifier_promotes_accessed_before_unaccessed_manifest_order_tiebreak() {
        let plan = plan_view(&multi_kind_manifest());
        // Files in manifest (path-sorted) order: Cargo.toml, src/lib.rs, src/main.rs.
        let mut s = Solidifier::new(&plan);

        // Access src/main.rs (last file in manifest order) — it must jump the
        // queue ahead of the un-accessed Cargo.toml / src/lib.rs.
        s.record_access("src/main.rs");

        // Hot entry first.
        assert_eq!(s.next_to_promote().as_deref(), Some("src/main.rs"));
        // Then the rest in manifest order.
        assert_eq!(s.next_to_promote().as_deref(), Some("Cargo.toml"));
        assert_eq!(s.next_to_promote().as_deref(), Some("src/lib.rs"));
        // Nothing left to hand out.
        assert_eq!(s.next_to_promote(), None);
    }

    #[test]
    fn solidifier_multiple_hot_entries_use_manifest_order_within_band() {
        let plan = plan_view(&multi_kind_manifest());
        let mut s = Solidifier::new(&plan);

        // Two hot entries — access the later one first; manifest order still
        // decides within the hot band (deterministic, not access-time-ordered).
        s.record_access("src/main.rs");
        s.record_access("Cargo.toml");

        assert_eq!(s.next_to_promote().as_deref(), Some("Cargo.toml"));
        assert_eq!(s.next_to_promote().as_deref(), Some("src/main.rs"));
        // Then the cold one.
        assert_eq!(s.next_to_promote().as_deref(), Some("src/lib.rs"));
        assert_eq!(s.next_to_promote(), None);
    }

    #[test]
    fn record_access_is_idempotent() {
        let plan = plan_view(&multi_kind_manifest());
        let mut s = Solidifier::new(&plan);

        s.record_access("src/lib.rs");
        s.record_access("src/lib.rs");
        s.record_access("src/lib.rs");

        // Still exactly one hand-out for it, still first (it's the only hot one).
        assert_eq!(s.next_to_promote().as_deref(), Some("src/lib.rs"));
        assert_eq!(s.next_to_promote().as_deref(), Some("Cargo.toml"));
        assert_eq!(s.next_to_promote().as_deref(), Some("src/main.rs"));
        assert_eq!(s.next_to_promote(), None);
    }

    #[test]
    fn record_access_on_nonfile_or_unknown_is_ignored() {
        let plan = plan_view(&multi_kind_manifest());
        let mut s = Solidifier::new(&plan);

        // A dir, a symlink, and an unknown path — none is a promotable file.
        s.record_access("src"); // dir
        s.record_access("link"); // symlink
        s.record_access("does/not/exist");

        // No file got hot, so plain manifest order applies.
        assert_eq!(s.next_to_promote().as_deref(), Some("Cargo.toml"));
        assert_eq!(s.next_to_promote().as_deref(), Some("src/lib.rs"));
        assert_eq!(s.next_to_promote().as_deref(), Some("src/main.rs"));
    }

    #[test]
    fn is_fully_solid_flips_only_after_all_files_promoted() {
        let plan = plan_view(&multi_kind_manifest());
        let mut s = Solidifier::new(&plan);

        // Hand out + confirm them one at a time; solid only at the very end.
        while let Some(p) = s.next_to_promote() {
            assert!(!s.is_fully_solid(), "must not be solid mid-flight");
            s.mark_promoted(&p);
        }
        assert!(s.is_fully_solid(), "solid once all files confirmed");
    }

    #[test]
    fn handing_out_without_confirming_is_not_solid() {
        let plan = plan_view(&multi_kind_manifest());
        let mut s = Solidifier::new(&plan);

        // Drain next_to_promote (all handed out) but confirm none.
        while s.next_to_promote().is_some() {}
        assert_eq!(s.next_to_promote(), None);
        // Handed out != promoted: a clone could still fail, so NOT solid.
        assert!(!s.is_fully_solid());

        // Confirm all but one — still not solid.
        s.mark_promoted("Cargo.toml");
        s.mark_promoted("src/lib.rs");
        assert!(!s.is_fully_solid());
        s.mark_promoted("src/main.rs");
        assert!(s.is_fully_solid());
    }

    #[test]
    fn empty_view_is_trivially_fully_solid() {
        let plan = plan_view(&Manifest {
            version: 1,
            total_size: 0,
            entries: vec![],
        });
        let mut s = Solidifier::new(&plan);
        assert!(s.is_fully_solid());
        assert_eq!(s.next_to_promote(), None);
    }

    #[test]
    fn fake_backend_records_mount_fault_in_unmount() {
        let plan = plan_view(&multi_kind_manifest());
        let mut be = FakeBackend::new();
        let at = Path::new("/tmp/view-root");

        be.mount(&plan, at).unwrap();
        be.fault_in("Cargo.toml").unwrap();
        be.fault_in("src/main.rs").unwrap();
        // Idempotent on the faulted set: the call is recorded, the set isn't dup'd.
        be.fault_in("Cargo.toml").unwrap();
        be.unmount(at).unwrap();

        assert_eq!(be.mounted, vec![at.to_path_buf()]);
        assert_eq!(be.unmounted, vec![at.to_path_buf()]);
        assert_eq!(
            be.fault_calls,
            vec![
                "Cargo.toml".to_string(),
                "src/main.rs".to_string(),
                "Cargo.toml".to_string()
            ]
        );
        // Distinct faulted set deduped.
        assert_eq!(be.faulted.len(), 2);
        assert!(be.faulted.contains("Cargo.toml"));
        assert!(be.faulted.contains("src/main.rs"));
    }

    #[test]
    fn solidify_step_drives_promotion_in_order_with_fake_backend() {
        use std::collections::HashMap;

        // Real store (lightr-store is real and host-testable). Ingest content,
        // build a manifest whose digests point at the ingested objects.
        let tmp = tempfile::tempdir().unwrap();
        let store = lightr_store::Store::open(tmp.path()).unwrap();

        let contents: HashMap<&str, &[u8]> = HashMap::from([
            ("Cargo.toml", b"[package]" as &[u8]),
            ("src/lib.rs", b"pub fn lib() {}" as &[u8]),
            ("src/main.rs", b"fn main() {}" as &[u8]),
        ]);

        let d_cargo = store.put_bytes(contents["Cargo.toml"]).unwrap();
        let d_lib = store.put_bytes(contents["src/lib.rs"]).unwrap();
        let d_main = store.put_bytes(contents["src/main.rs"]).unwrap();

        let manifest = Manifest {
            version: 1,
            total_size: 0,
            entries: vec![
                Entry::Dir { path: "src".into() },
                Entry::File {
                    path: "Cargo.toml".into(),
                    mode: 0o644,
                    size: contents["Cargo.toml"].len() as u64,
                    digest: d_cargo,
                },
                Entry::File {
                    path: "src/lib.rs".into(),
                    mode: 0o644,
                    size: contents["src/lib.rs"].len() as u64,
                    digest: d_lib,
                },
                Entry::File {
                    path: "src/main.rs".into(),
                    mode: 0o644,
                    size: contents["src/main.rs"].len() as u64,
                    digest: d_main,
                },
            ],
        };

        let plan = plan_view(&manifest);
        let mut be = FakeBackend::new();
        let mut sol = Solidifier::new(&plan);
        let dest_root = tmp.path().join("view");

        // Make src/main.rs hot so it promotes first (out of manifest order).
        sol.record_access("src/main.rs");

        // Drive every step; collect the order the driver promotes in.
        let mut order = Vec::new();
        while let Some(p) = solidify_step(&plan, &mut be, &mut sol, &store, &dest_root).unwrap() {
            order.push(p);
        }

        // Hot first, then manifest order — the solidifier's documented policy,
        // observed end-to-end through the driver.
        assert_eq!(order, vec!["src/main.rs", "Cargo.toml", "src/lib.rs"]);

        // The driver faulted each promoted entry into the backend, in order.
        assert_eq!(be.fault_calls, order);

        // It CoW-cloned real content to disk for each file (store path proven).
        for (rel, bytes) in &contents {
            let landed = std::fs::read(dest_root.join(rel)).unwrap();
            assert_eq!(&landed, bytes, "content for {rel} must match the store");
        }

        // Fully solid, mount can evaporate.
        assert!(sol.is_fully_solid());
    }
}
