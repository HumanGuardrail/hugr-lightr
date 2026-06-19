//! A25, A25b acceptance tests (docker compat + exit-code law).

use crate::common::lightr_cmd;
use std::fs;
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// A25 — docker compat
//
// `lightr docker build -t @t/d <ctx>` → exit 0 + stderr contains "lightr build"
// `lightr docker images`               → lists @t/d
// `lightr docker frobnicate`           → exit 2 + stderr contains "unsupported"
//                                        and mentions supported verbs
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a25_docker_compat() {
    let home = TempDir::new().unwrap();
    let ctx = TempDir::new().unwrap();

    // Minimal Dockerfile for the docker build test.
    fs::write(
        ctx.path().join("Dockerfile"),
        "FROM scratch\nCOPY data.txt /data.txt\n",
    )
    .unwrap();
    fs::write(ctx.path().join("data.txt"), b"a25").unwrap();

    // ── docker build → exit 0 + stderr transparency note ───────────────────
    let build_out = lightr_cmd(home.path())
        .args([
            "docker",
            "build",
            "-t",
            "@t/d",
            ctx.path().to_str().unwrap(),
        ])
        .output()
        .expect("docker build must not fail to spawn");
    assert_eq!(
        build_out.status.code().unwrap_or(-1),
        0,
        "docker build must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&build_out.stderr)
    );
    // Transparency note: stderr must say it ran "lightr build" (per §4).
    let build_stderr = String::from_utf8_lossy(&build_out.stderr).to_lowercase();
    assert!(
        build_stderr.contains("lightr build") || build_stderr.contains("lightr-build"),
        "docker build stderr must mention 'lightr build' (transparency note); got:\n{}",
        String::from_utf8_lossy(&build_out.stderr)
    );

    // ── docker images → lists @t/d ──────────────────────────────────────────
    let images_out = lightr_cmd(home.path())
        .args(["docker", "images"])
        .output()
        .expect("docker images must not fail to spawn");
    assert_eq!(
        images_out.status.code().unwrap_or(-1),
        0,
        "docker images must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&images_out.stderr)
    );
    let images_stdout = String::from_utf8_lossy(&images_out.stdout);
    assert!(
        images_stdout.contains("@t/d") || images_stdout.contains("t/d"),
        "docker images must list @t/d after docker build; got:\n{}",
        images_stdout
    );

    // ── docker frobnicate → exit 2 + "unsupported" + supported list ─────────
    let frob_out = lightr_cmd(home.path())
        .args(["docker", "frobnicate"])
        .output()
        .expect("docker frobnicate must not fail to spawn");
    assert_eq!(
        frob_out.status.code().unwrap_or(-1),
        2,
        "docker frobnicate must exit 2 (unsupported subcommand)"
    );
    let frob_stderr = String::from_utf8_lossy(&frob_out.stderr).to_lowercase();
    assert!(
        frob_stderr.contains("unsupported"),
        "docker frobnicate stderr must contain 'unsupported'; got:\n{}",
        String::from_utf8_lossy(&frob_out.stderr)
    );
    // Must name at least one of the supported verbs.
    let mentions_supported = frob_stderr.contains("build")
        || frob_stderr.contains("run")
        || frob_stderr.contains("pull")
        || frob_stderr.contains("images")
        || frob_stderr.contains("ps")
        || frob_stderr.contains("compose");
    assert!(
        mentions_supported,
        "docker frobnicate stderr must mention supported verbs (build|run|pull|images|ps|compose); got:\n{}",
        String::from_utf8_lossy(&frob_out.stderr)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A25b — docker subcommand exit-code law
//
// `lightr docker ps` → must exit 0 (translates to `ps`).
// `lightr docker pull alpine` → exit 0 or 1 (network may be absent), never 2.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a25b_docker_ps_and_pull_exit_law() {
    let home = TempDir::new().unwrap();

    // docker ps → translates to `lightr ps` → exit 0 always.
    let ps_out = lightr_cmd(home.path())
        .args(["docker", "ps"])
        .output()
        .expect("docker ps must not fail to spawn");
    assert_eq!(
        ps_out.status.code().unwrap_or(-1),
        0,
        "docker ps must exit 0 (translates to lightr ps); stderr:\n{}",
        String::from_utf8_lossy(&ps_out.stderr)
    );

    // docker pull: exit 0 (net available) or 1 (no net); NEVER 2.
    let pull_out = lightr_cmd(home.path())
        .args(["docker", "pull", "alpine"])
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .expect("docker pull must not fail to spawn");
    let pull_code = pull_out.status.code().unwrap_or(-1);
    assert!(
        pull_code == 0 || pull_code == 1,
        "docker pull must exit 0 or 1; got exit={pull_code} stderr:\n{}",
        String::from_utf8_lossy(&pull_out.stderr)
    );
    assert_ne!(
        pull_code, 2,
        "docker pull must NEVER exit 2 for a valid image ref"
    );
}
