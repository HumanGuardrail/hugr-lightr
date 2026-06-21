//! WP-DF-FLAKE regression: `TempDirGuard::new()` must hand out a UNIQUE,
//! exclusively-owned work dir to every concurrent caller.
//!
//! The flake was a nanos-ONLY name: under heavy parallel load two concurrent
//! `build()` calls (or a build + its `COPY --from` stage-materialize guard
//! racing another build) could read the same coarse clock value, derive the same
//! path, and silently SHARE one dir via `create_dir_all` — one clobbered the
//! other → `NotFound(<digest>)` in a multi-stage `COPY --from`. These tests
//! assert the post-fix guarantee directly (no clock luck needed): a tight burst
//! of guards, and a many-thread parallel burst, all get DISTINCT, real dirs.
use super::*;
use std::collections::HashSet;

#[test]
fn sequential_burst_yields_distinct_existing_dirs() {
    // A tight loop hammers the clock faster than its resolution — pre-fix this
    // collided on identical nanos. Post-fix the atomic counter disambiguates, so
    // every path is unique AND was actually created (exclusive `create_dir`).
    let mut seen = HashSet::new();
    let mut guards = Vec::new();
    for _ in 0..2_000 {
        let g = TempDirGuard::new().expect("guard create must succeed");
        assert!(g.path.is_dir(), "guard path must be a real, created dir");
        assert!(
            seen.insert(g.path.clone()),
            "duplicate work dir handed out: {:?} (collision-proofing regressed)",
            g.path
        );
        guards.push(g); // hold so Drop doesn't free a name for reuse mid-test
    }
}

#[test]
fn parallel_burst_yields_distinct_dirs() {
    // The real failure mode: MANY threads creating guards at once (mirrors the
    // loaded gate spawning concurrent builds / COPY --from materializes). Every
    // path returned across all threads must be distinct and exist.
    use std::sync::{Arc, Mutex};
    use std::thread;

    let collected: Arc<Mutex<Vec<std::path::PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    // Keep guards alive until the end so no name is freed + reusable mid-run.
    let alive: Arc<Mutex<Vec<TempDirGuard>>> = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for _ in 0..16 {
        let collected = Arc::clone(&collected);
        let alive = Arc::clone(&alive);
        handles.push(thread::spawn(move || {
            for _ in 0..200 {
                let g = TempDirGuard::new().expect("guard create must succeed");
                assert!(g.path.is_dir(), "guard path must be a real, created dir");
                collected.lock().unwrap().push(g.path.clone());
                alive.lock().unwrap().push(g);
            }
        }));
    }
    for h in handles {
        h.join().expect("worker thread panicked");
    }

    let paths = collected.lock().unwrap();
    let unique: HashSet<_> = paths.iter().collect();
    assert_eq!(
        unique.len(),
        paths.len(),
        "concurrent TempDirGuard::new() handed out a duplicate work dir \
         (collision-proofing regressed → multi-stage NotFound under load)"
    );
}
