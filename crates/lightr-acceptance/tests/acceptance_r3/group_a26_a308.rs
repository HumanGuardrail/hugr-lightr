//! A26, A308 acceptance tests.

use super::helpers::*;
use crate::common::lightr_cmd;
use std::fs;
use tempfile::TempDir;

// ─────────────────────────────────────────────────────────────────────────────
// A26 — build determinism flag
//
// A Dockerfile with `RUN /bin/sh -c 'date > ts.txt'`.
// `build -t @t/c --explain <ctx>` → exit 0 (build still succeeds) and
// stderr flags the RUN as non-reproducible (contains "date" or "non-reprodu").
// The `step_reads_clock_or_net` heuristic in lightr-build flags the `date`
// command.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a26_build_determinism_flag() {
    let home = TempDir::new().unwrap();
    let ctx = TempDir::new().unwrap();

    // Dockerfile with a RUN that reads the clock.
    fs::write(
        ctx.path().join("Dockerfile"),
        "FROM scratch\nRUN /bin/sh -c 'date > ts.txt'\n",
    )
    .unwrap();

    let out = lightr_cmd(home.path())
        .args([
            "build",
            "-t",
            "@t/c",
            "--explain",
            ctx.path().to_str().unwrap(),
        ])
        .output()
        .expect("build --explain must not fail to spawn");

    // Build must succeed (exit 0); non-reproducible steps are RECORDED, not failed.
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "build --explain must exit 0 (determinism warnings do not fail the build); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // stderr must flag the non-reproducible RUN.
    // The heuristic matches "date" in the argv, so stderr should mention "date"
    // or use the "non-reprodu" / "non-reproducible" wording.
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    let flagged = stderr.contains("date")
        || stderr.contains("non-reprodu")
        || stderr.contains("non_reprodu")
        || stderr.contains("clock")
        || stderr.contains("reproducible");
    assert!(
        flagged,
        "build --explain stderr must flag the 'date' RUN as non-reproducible; got:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A-308 — restart via OS supervisor (F-308): install GENERATES a unit (no
// daemon), list shows it, uninstall removes it, and ZERO lightr daemons are
// ever resident (the A4 no-daemon invariant must still hold — we only wrote a
// file). The generated unit must be supervisor-valid (parse it back).
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn a308_supervise_install_list_uninstall_no_daemon() {
    let home = TempDir::new().unwrap();
    let units = home.path().join("units");

    // install --name web --restart on-failure:3 --dir . -- /bin/echo hi
    lightr_cmd(home.path())
        .args([
            "supervise",
            "install",
            "--name",
            "web",
            "--restart",
            "on-failure:3",
            "--dir",
            ".",
            "--",
            "/bin/echo",
            "hi",
        ])
        .assert()
        .success();

    // A unit file landed under ~/.lightr/units/ with the platform extension.
    #[cfg(target_os = "macos")]
    let unit_path = units.join("web.plist");
    #[cfg(target_os = "linux")]
    let unit_path = units.join("web.service");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        assert!(
            unit_path.exists() && unit_path.is_file(),
            "supervise install must write a unit at {}",
            unit_path.display()
        );
        let text = fs::read_to_string(&unit_path).unwrap();

        // Unit must be supervisor-valid (parse/lint it back).
        #[cfg(target_os = "macos")]
        {
            // `plutil -lint` parses the plist; non-zero = malformed.
            let lint = std::process::Command::new("plutil")
                .arg("-lint")
                .arg(&unit_path)
                .output()
                .expect("plutil must be present on macOS");
            assert!(
                lint.status.success(),
                "generated plist must pass plutil -lint:\n{}\n--- unit ---\n{text}",
                String::from_utf8_lossy(&lint.stdout)
            );
            // on-failure → KeepAlive { SuccessfulExit = false }.
            assert!(
                text.contains("SuccessfulExit"),
                "on-failure ⇒ SuccessfulExit"
            );
            assert!(text.contains("<key>RunAtLoad</key>"));
        }
        #[cfg(target_os = "linux")]
        {
            // No systemd-analyze in CI guaranteed; assert the structural law.
            assert!(text.contains("[Service]"));
            assert!(text.contains("Restart=on-failure"));
            assert!(text.contains("ExecStart=/bin/echo hi"));
        }
    }

    // list shows the unit by name.
    let listed = lightr_cmd(home.path())
        .args(["supervise", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&listed).lines().any(|l| l == "web"),
        "supervise list must show 'web'; got:\n{}",
        String::from_utf8_lossy(&listed)
    );

    // The A4 invariant: install/list generated a FILE and nothing resident.
    // No control sockets, no run/ supervisor dirs, no *.pid under LIGHTR_HOME.
    #[cfg(unix)]
    {
        let entries = walkdir(home.path());
        for path in &entries {
            let meta = fs::symlink_metadata(path).unwrap();
            let ft = meta.file_type();
            use std::os::unix::fs::FileTypeExt;
            assert!(
                !ft.is_socket(),
                "supervise must leave no socket: {}",
                path.display()
            );
            assert!(
                !ft.is_fifo(),
                "supervise must leave no FIFO: {}",
                path.display()
            );
            if let Some(name) = path.file_name() {
                assert!(
                    !name.to_string_lossy().ends_with(".pid"),
                    "supervise must leave no pidfile: {}",
                    path.display()
                );
            }
        }
    }
    let run_dir = home.path().join("run");
    assert!(
        !run_dir.exists()
            || fs::read_dir(&run_dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "supervise must NOT create a resident run/ supervisor (no daemon of ours)"
    );

    // uninstall --name web removes the unit.
    lightr_cmd(home.path())
        .args(["supervise", "uninstall", "--name", "web"])
        .assert()
        .success();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    assert!(
        !unit_path.exists(),
        "supervise uninstall must remove the unit at {}",
        unit_path.display()
    );

    // uninstall again ⇒ honest error (the unit is gone), never a silent success.
    lightr_cmd(home.path())
        .args(["supervise", "uninstall", "--name", "web"])
        .assert()
        .failure();
}

#[test]
fn a308_supervise_install_rejects_bad_policy() {
    let home = TempDir::new().unwrap();
    // A garbage restart policy must fail closed (exit 2, usage-class), and write
    // nothing under LIGHTR_HOME.
    lightr_cmd(home.path())
        .args([
            "supervise",
            "install",
            "--name",
            "bad",
            "--restart",
            "sometimes",
            "--dir",
            ".",
            "--",
            "/bin/true",
        ])
        .assert()
        .failure()
        .code(2);
    assert!(
        !home.path().join("units").join("bad.plist").exists()
            && !home.path().join("units").join("bad.service").exists(),
        "a rejected policy must write no unit file"
    );
}

#[test]
fn a308_supervise_list_empty_is_clean() {
    let home = TempDir::new().unwrap();
    // No units installed ⇒ list exits 0 with empty output (not an error).
    let out = lightr_cmd(home.path())
        .args(["supervise", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&out).trim().is_empty(),
        "empty supervise list must print nothing; got:\n{}",
        String::from_utf8_lossy(&out)
    );
}
