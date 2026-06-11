//! Shared fixture helpers for A1–A8.
//!
//! Rules:
//! - `fixture_tree(root)` builds a deterministic nested workspace for tests.
//! - `lightr_cmd(home)` returns an `assert_cmd::Command` bound to the binary
//!   with `LIGHTR_HOME` set; callers set `current_dir` as needed.
//! - Nothing here touches `$HOME`.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::Command;

/// Build the canonical fixture tree under `root`.
///
/// Layout guarantees (spec §8 / WP-6 authoring law):
/// - ~200 regular files of ~1 KiB each, spread across 3 levels of dirs.
/// - One file with execute bit set (0o755).
/// - One symlink pointing at a sibling file.
/// - One empty directory.
/// - One file ≥ 8 MiB.
pub fn fixture_tree(root: &Path) {
    // Top-level dirs: a/b/c three levels deep
    let dirs = [
        "level1/sub1/deep1",
        "level1/sub1/deep2",
        "level1/sub2/deep1",
        "level2/sub1/deep1",
        "level2/sub2/deep1",
        "level3/sub1/deep1",
    ];
    for d in &dirs {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }

    // Empty dir (no files inside)
    std::fs::create_dir_all(root.join("empty_dir")).unwrap();

    // ~200 regular files of ~1 KiB spread across dirs
    let all_dirs = [
        "level1/sub1/deep1",
        "level1/sub1/deep2",
        "level1/sub2/deep1",
        "level2/sub1/deep1",
        "level2/sub2/deep1",
        "level3/sub1/deep1",
    ];
    let payload_1k = "x".repeat(1024);
    let mut file_idx = 0usize;
    'outer: for dir in all_dirs.iter().cycle() {
        let fname = format!("file_{:04}.txt", file_idx);
        std::fs::write(root.join(dir).join(&fname), payload_1k.as_bytes()).unwrap();
        file_idx += 1;
        if file_idx >= 200 {
            break 'outer;
        }
    }

    // One execute-bit file (0o755)
    let exec_path = root.join("exec_script.sh");
    std::fs::write(&exec_path, b"#!/bin/sh\necho hello\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&exec_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    // One symlink to a sibling (level1/sub1/deep1/file_0000.txt)
    #[allow(unused_variables)]
    let link_target = Path::new("file_0000.txt");
    #[allow(unused_variables)]
    let link_path = root.join("level1/sub1/deep1/symlink_to_first.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(link_target, &link_path).unwrap();

    // One file ≥ 8 MiB
    let big_payload = vec![0xABu8; 8 * 1024 * 1024 + 1024];
    std::fs::write(root.join("bigfile.bin"), &big_payload).unwrap();
}

/// Return a `Command` for the `lightr` binary with `LIGHTR_HOME` set to `home`.
///
/// Proxy env vars are removed so tests are not accidentally routed through
/// any ambient proxy (A6 sets them explicitly as a canary).
pub fn lightr_cmd(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("lightr").unwrap();
    cmd.env("LIGHTR_HOME", home);
    cmd.env_remove("HTTP_PROXY");
    cmd.env_remove("HTTPS_PROXY");
    cmd.env_remove("ALL_PROXY");
    cmd.env_remove("http_proxy");
    cmd.env_remove("https_proxy");
    cmd.env_remove("all_proxy");
    cmd
}
