//! `mod r1_tests` — gc end-to-end + undo + bisect stub tests.
#![cfg(test)]

use crate::index::gc::gc;
use crate::index::hydrate::hydrate;
use crate::index::snapshot::snapshot;
use lightr_store::Store;
use std::fs;
use tempfile::TempDir;

use super::super::super::TEST_ENV_LOCK;

// Share the process-global lock defined at crate level so this module and
// the `tests` module serialize all LIGHTR_HOME mutations together.
fn with_lightr_home(tmp: &TempDir) -> std::sync::MutexGuard<'static, ()> {
    let guard = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("LIGHTR_HOME", tmp.path());
    guard
}

// -----------------------------------------------------------------------
// Store-dependent gc end-to-end tests
// -----------------------------------------------------------------------

/// dry_run_reachable: snapshot a tree twice (two ref-log versions) →
/// gc(dry_run=true, 0) must report swept==0 and objects_total≥2 (both
/// manifest objects are reachable via the ref-log).
#[test]
fn gc_end_to_end_dry_run_reachable() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);

    // Store lives under LIGHTR_HOME/store; the snapshot fn writes the index
    // under LIGHTR_HOME/index — both use the same LIGHTR_HOME.
    let store_root = home.path().join("store");
    let store = Store::open(&store_root).unwrap();

    // Root tree: two files.
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("a.txt"), b"content-v1").unwrap();
    fs::write(root.path().join("b.txt"), b"shared").unwrap();

    // Version 1
    snapshot(root.path(), &store, "main").unwrap();

    // Mutate a file to produce a second manifest with a different digest.
    fs::write(root.path().join("a.txt"), b"content-v2").unwrap();

    // Version 2
    snapshot(root.path(), &store, "main").unwrap();

    // Dry-run gc: nothing should be swept because all objects are reachable
    // via the ref-log (both manifest objects + all file objects).
    let report = gc(&store, true, 0).unwrap();

    assert_eq!(
        report.swept, 0,
        "dry-run gc must not sweep any reachable object (swept={})",
        report.swept
    );
    // We have at least 2 manifest blobs + at least 2 file blobs (a.txt v1 + v2)
    // + 1 shared b.txt blob = at least 5 objects, but ≥2 is the contract minimum.
    assert!(
        report.objects_total >= 2,
        "expected objects_total≥2 after two snapshots, got {}",
        report.objects_total
    );
    // reachable + swept == objects_total
    assert_eq!(
        report.reachable + report.swept,
        report.objects_total,
        "reachable+swept must equal objects_total"
    );
    // bytes_freed must be 0 in dry-run (no mutations).
    assert_eq!(report.bytes_freed, 0, "dry-run must free no bytes");
}

/// sweep_orphan: put_bytes an orphan blob not referenced by any ref/AC;
/// gc(dry_run=false, 0) → orphan !exists() afterward, AND the live ref
/// still hydrates byte-identical.
#[test]
fn gc_end_to_end_sweep_orphan() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);

    let store_root = home.path().join("store");
    let store = Store::open(&store_root).unwrap();

    // Snapshot a live tree.
    let root = TempDir::new().unwrap();
    let live_content = b"live-file-content";
    fs::write(root.path().join("live.txt"), live_content).unwrap();
    let snap = snapshot(root.path(), &store, "main").unwrap();
    let manifest_digest = snap.root;

    // Put an orphan blob — NOT referenced by any ref, AC, or manifest.
    let orphan_data = b"orphan-blob-unreachable";
    let orphan_digest = store.put_bytes(orphan_data).unwrap();
    assert!(store.exists(&orphan_digest), "orphan must exist before gc");

    // Run real gc sweep.
    let report = gc(&store, false, 0).unwrap();

    // The orphan must have been swept.
    assert!(
        !store.exists(&orphan_digest),
        "gc must have removed the orphan blob"
    );
    assert!(report.swept >= 1, "gc must report ≥1 swept object");

    // The live manifest and file objects must still be intact.
    assert!(
        store.exists(&manifest_digest),
        "live manifest object must survive gc"
    );

    // Mini roundtrip: hydrate into a fresh dir and verify byte-identity.
    let dest = TempDir::new().unwrap();
    let hr = hydrate(dest.path(), &store, "main").unwrap();
    assert_eq!(hr.root, manifest_digest, "hydrated root digest must match");

    let got = fs::read(dest.path().join("live.txt")).unwrap();
    assert_eq!(
        got.as_slice(),
        live_content,
        "hydrated file content must be byte-identical"
    );
}

// -----------------------------------------------------------------------
// gc Fix 1: hard-killed run dirs are reclaimed via coarse-age heuristic
// -----------------------------------------------------------------------

/// A run dir with status "running" and a pid file, whose mtime is backdated
/// far enough (> 24 h) to exceed the hard-killed floor, MUST be reclaimed.
/// Unix-only: uses `touch -t` to backdate mtime without extra dependencies.
#[test]
#[cfg(unix)]
fn gc_reclaims_hard_killed_run_dir() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);

    let store_root = home.path().join("store");
    let store = Store::open(&store_root).unwrap();

    // Create a run dir under LIGHTR_HOME/run/fake-run-001
    let run_root = home.path().join("run");
    fs::create_dir_all(&run_root).unwrap();
    let run_dir = run_root.join("fake-run-001");
    fs::create_dir_all(&run_dir).unwrap();

    // Write status = "running" (not "exited")
    fs::write(run_dir.join("status"), b"running").unwrap();
    // Write a pid file with a high-numbered PID that is certainly dead.
    // We pick 999999999 — well above Linux/macOS PID_MAX — so it cannot
    // be alive. The coarse-age path does NOT use kill(0); it only checks
    // presence of the pid file.
    fs::write(run_dir.join("pid"), b"999999999").unwrap();

    // Backdate the dir's mtime to 202001010000 (2020-01-01 00:00) via
    // `touch -t` — no extra crate dependency required.
    let status = std::process::Command::new("touch")
        .args(["-t", "202001010000", run_dir.to_str().unwrap()])
        .status()
        .expect("touch must be available on unix");
    assert!(status.success(), "touch -t backdating must succeed");

    // gc with min_age=0: the hard-killed floor (24 h) is the binding limit.
    let report = gc(&store, false, 0).unwrap();
    assert!(
        report.run_dirs_removed >= 1,
        "gc must reclaim the hard-killed run dir (run_dirs_removed={})",
        report.run_dirs_removed
    );
    assert!(
        !run_dir.exists(),
        "hard-killed run dir must be removed from disk"
    );
}

/// A run dir with status "running" and a pid file, but whose mtime is only
/// 1 h ago (< 24 h hard-killed floor), must NOT be reclaimed.
#[test]
fn gc_does_not_reclaim_recent_running_dir() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);

    let store_root = home.path().join("store");
    let store = Store::open(&store_root).unwrap();

    let run_root = home.path().join("run");
    fs::create_dir_all(&run_root).unwrap();
    let run_dir = run_root.join("recent-run-001");
    fs::create_dir_all(&run_dir).unwrap();

    fs::write(run_dir.join("status"), b"running").unwrap();
    // Use current process PID — definitely alive.
    fs::write(
        run_dir.join("pid"),
        std::process::id().to_string().as_bytes(),
    )
    .unwrap();
    // mtime is "now" (created just above) — far below 24 h floor.

    let report = gc(&store, false, 0).unwrap();
    assert_eq!(
        report.run_dirs_removed, 0,
        "gc must NOT reclaim a recently-created running dir"
    );
    assert!(
        run_dir.exists(),
        "recent running dir must still exist after gc"
    );
}

/// A run dir with status "running" but NO pid file must NOT be reclaimed
/// (conservative: no pid file = unknown origin).
#[test]
fn gc_does_not_reclaim_running_dir_without_pid_file() {
    let home = TempDir::new().unwrap();
    let _env_guard = with_lightr_home(&home);

    let store_root = home.path().join("store");
    let store = Store::open(&store_root).unwrap();

    let run_root = home.path().join("run");
    fs::create_dir_all(&run_root).unwrap();
    let run_dir = run_root.join("nopid-run-001");
    fs::create_dir_all(&run_dir).unwrap();

    fs::write(run_dir.join("status"), b"running").unwrap();
    // Deliberately NO pid file.

    let report = gc(&store, false, 0).unwrap();
    assert_eq!(
        report.run_dirs_removed, 0,
        "gc must not reclaim a running dir that has no pid file"
    );
    assert!(
        run_dir.exists(),
        "running dir without pid file must survive gc"
    );
}

#[test]
fn undo_restores_previous_version() {
    // snapshot v1, snapshot v2, undo → hydrate yields v1 bytes.
}

#[test]
fn undo_no_history_returns_ref_not_found() {
    // fresh ref with only one entry → undo → Err(RefNotFound).
}

#[test]
fn bisect_4_snapshots_finds_boundary() {
    // 4 snapshots: v0..v3, marker file absent in v0..v1, present in v2..v3.
    // cmd = ["sh", "-c", "test ! -f bad.marker"]
    // bisect finds the oldest-bad index (2).
}

#[test]
fn bisect_endpoints_invalid_returns_error() {
    // Both endpoints good → Err(InvalidRef("bisect: endpoints not bad/good")).
}

#[test]
fn bisect_need_at_least_2_versions() {
    // ref_log with only 1 entry → Err(InvalidRef("bisect: need ≥2 versions")).
}
