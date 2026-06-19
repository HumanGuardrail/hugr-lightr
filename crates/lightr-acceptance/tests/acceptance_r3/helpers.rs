//! Non-`#[test]` helpers shared by all acceptance_r3 sub-modules.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Polling helper
// ─────────────────────────────────────────────────────────────────────────────

/// Polls `pred` every 100 ms until it returns `true` or `timeout` expires.
pub(super) fn poll_until<F: FnMut() -> bool>(timeout: Duration, mut pred: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Parse `steps=<n> cached=<n>` from stdout
// ─────────────────────────────────────────────────────────────────────────────

pub(super) fn parse_build_report(stdout: &[u8]) -> (u64, u64) {
    let text = String::from_utf8_lossy(stdout);
    let mut steps: Option<u64> = None;
    let mut cached: Option<u64> = None;
    for tok in text.split_whitespace() {
        if let Some(v) = tok.strip_prefix("steps=") {
            steps = v.parse().ok();
        }
        if let Some(v) = tok.strip_prefix("cached=") {
            cached = v.parse().ok();
        }
    }
    (
        steps.unwrap_or_else(|| panic!("could not parse 'steps=<n>' from stdout:\n{}", text)),
        cached.unwrap_or_else(|| panic!("could not parse 'cached=<n>' from stdout:\n{}", text)),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Recursively collect every path under `root` (for the no-daemon sweep).
#[cfg(unix)]
pub(super) fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.clone());
            if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                stack.push(p);
            }
        }
    }
    out
}

/// Assert that `path` (relative to `root`) exists as a regular file.
#[allow(dead_code)]
pub(super) fn assert_file_exists(root: &Path, rel: &str) {
    let p = root.join(rel);
    assert!(
        p.exists() && p.is_file(),
        "expected regular file at {}: not found",
        p.display()
    );
}
