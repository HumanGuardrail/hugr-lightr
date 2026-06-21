//! WP-IMAGE-VERBS acceptance — end-to-end CLI contracts for the top-level
//! Docker image-management verbs mapped onto the lightr ref registry:
//! `images`, `rmi`, `tag`, `history`, `commit`.
//!
//! LEAD DESIGN (frozen): a Docker "image" = a named lightr ref. These tests
//! drive the real `lightr` binary (codifying exit codes + stdout/stderr shape
//! that per-crate tests miss). Every test uses its own tempdir LIGHTR_HOME and
//! never touches `~` (house rule).
//!
//! Gate: cargo test -p lightr-acceptance --test image_verbs.

#[path = "common/mod.rs"]
#[allow(dead_code)]
mod common;

use common::lightr_cmd;
use tempfile::TempDir;

/// Create a ref named `name` from a tiny one-file workspace via `snapshot`.
fn make_image(home: &std::path::Path, name: &str) -> TempDir {
    let ws = TempDir::new().unwrap();
    std::fs::write(ws.path().join("file.txt"), b"payload bytes").unwrap();
    let out = lightr_cmd(home)
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", name])
        .output()
        .expect("snapshot spawns");
    assert!(
        out.status.success(),
        "snapshot {name} must succeed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    ws
}

fn run_lightr(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    lightr_cmd(home).args(args).output().expect("lightr spawns")
}

// ── images ───────────────────────────────────────────────────────────────────

#[test]
fn images_lists_named_ref_with_columns() {
    let home = TempDir::new().unwrap();
    let _ws = make_image(home.path(), "alpha");

    let out = run_lightr(home.path(), &["images"]);
    assert_eq!(out.status.code(), Some(0), "images exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("REPOSITORY") && stdout.contains("IMAGE ID") && stdout.contains("SIZE"),
        "header carries Docker columns; got:\n{stdout}"
    );
    assert!(stdout.contains("alpha"), "the ref appears as a repository");
}

#[test]
fn images_quiet_prints_only_ids() {
    let home = TempDir::new().unwrap();
    let _ws = make_image(home.path(), "beta");

    let out = run_lightr(home.path(), &["images", "-q"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("REPOSITORY"), "quiet ⇒ no header");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "one image ⇒ one id line");
    assert_eq!(lines[0].len(), 12, "quiet prints the 12-char short id");
}

#[test]
fn images_json_is_an_array() {
    let home = TempDir::new().unwrap();
    let _ws = make_image(home.path(), "gamma");

    let out = run_lightr(home.path(), &["--json", "images"]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("images --json is valid JSON");
    let arr = v.as_array().expect("images --json is an array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["repository"], "gamma");
    assert!(arr[0]["size"].as_u64().unwrap() > 0);
}

// ── rmi ──────────────────────────────────────────────────────────────────────

#[test]
fn rmi_untags_existing_image() {
    let home = TempDir::new().unwrap();
    let _ws = make_image(home.path(), "doomed");

    let out = run_lightr(home.path(), &["rmi", "doomed"]);
    assert_eq!(out.status.code(), Some(0), "rmi of existing image exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Untagged: doomed"), "Docker Untagged line");

    // It is gone from images now.
    let after = run_lightr(home.path(), &["images"]);
    assert!(!String::from_utf8_lossy(&after.stdout).contains("doomed"));
}

#[test]
fn rmi_missing_image_is_exit_1() {
    let home = TempDir::new().unwrap();
    let out = run_lightr(home.path(), &["rmi", "ghost"]);
    assert_eq!(out.status.code(), Some(1), "missing image ⇒ exit 1");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("No such image: ghost"),
        "Docker No-such-image message"
    );
}

// ── tag ──────────────────────────────────────────────────────────────────────

#[test]
fn tag_aliases_then_images_shows_both() {
    let home = TempDir::new().unwrap();
    let _ws = make_image(home.path(), "orig");

    let out = run_lightr(home.path(), &["tag", "orig", "alias"]);
    assert_eq!(out.status.code(), Some(0), "tag exits 0");

    let imgs = run_lightr(home.path(), &["images"]);
    let stdout = String::from_utf8_lossy(&imgs.stdout);
    assert!(
        stdout.contains("orig") && stdout.contains("alias"),
        "both shown"
    );
}

#[test]
fn tag_missing_src_is_exit_1() {
    let home = TempDir::new().unwrap();
    let out = run_lightr(home.path(), &["tag", "nope", "alias"]);
    assert_eq!(out.status.code(), Some(1), "missing src ⇒ exit 1");
    assert!(String::from_utf8_lossy(&out.stderr).contains("No such image: nope"));
}

// ── history ──────────────────────────────────────────────────────────────────

#[test]
fn history_shows_version_log_and_honest_note() {
    let home = TempDir::new().unwrap();
    let ws = make_image(home.path(), "histimg");
    // A second snapshot of the same ref appends a version.
    std::fs::write(ws.path().join("file.txt"), b"changed payload bytes!!!").unwrap();
    let out = lightr_cmd(home.path())
        .current_dir(ws.path())
        .args(["snapshot", "--dir", ".", "--name", "histimg"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let hist = run_lightr(home.path(), &["history", "histimg"]);
    assert_eq!(hist.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&hist.stdout);
    assert!(stdout.contains("IMAGE ID") && stdout.contains("CREATED"));
    // Two versions ⇒ at least two data rows under the header.
    let rows = stdout.lines().filter(|l| !l.is_empty()).count();
    assert!(rows >= 3, "header + 2 version rows; got {rows}");
    // The honest layer-gap note goes to STDERR.
    assert!(
        String::from_utf8_lossy(&hist.stderr).contains("per-instruction layer"),
        "honest layer-gap note on stderr"
    );
}

#[test]
fn history_missing_image_is_exit_1() {
    let home = TempDir::new().unwrap();
    let out = run_lightr(home.path(), &["history", "ghost"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("No such image: ghost"));
}

// ── commit ───────────────────────────────────────────────────────────────────

#[test]
fn commit_missing_container_is_exit_1() {
    let home = TempDir::new().unwrap();
    let out = run_lightr(home.path(), &["commit", "ghost-container"]);
    assert_eq!(out.status.code(), Some(1), "missing container ⇒ exit 1");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("No such container: ghost-container"),
        "Docker No-such-container message"
    );
}

#[test]
fn commit_snapshots_a_container_rootfs() {
    let home = TempDir::new().unwrap();

    // A "container" is a detached run: a directory `<home>/run/<id>` that the
    // resolver matches by exact id, with a `rootfs/` subtree. We fabricate it
    // directly (engine-independent) so the commit path is exercised end-to-end
    // — exactly the on-disk shape a detached run leaves behind.
    let id = "1717600000000000000-7";
    let rootfs = home.path().join("run").join(id).join("rootfs");
    std::fs::create_dir_all(&rootfs).unwrap();
    std::fs::write(rootfs.join("committed.txt"), b"from container").unwrap();

    let out = run_lightr(home.path(), &["commit", id, "snapshotted"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "commit of a resolvable container exits 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("sha256:"),
        "commit prints the new image id (Docker shape)"
    );

    // The new image appears in the listing.
    let imgs = run_lightr(home.path(), &["images"]);
    assert!(String::from_utf8_lossy(&imgs.stdout).contains("snapshotted"));
}
