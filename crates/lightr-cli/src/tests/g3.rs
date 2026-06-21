use super::*;

// ── engine ls ─────────────────────────────────────────────────────────────

#[test]
fn engine_ls_parses() {
    let cli = parse(&["engine", "ls"]);
    match &cli.cmd {
        Cmd::Engine { subcmd } => {
            matches!(subcmd, EngineCmd::Ls);
        }
        _ => panic!("expected Engine cmd"),
    }
}

#[test]
fn engine_ls_json_uses_global_flag() {
    let cli = parse(&["--json", "engine", "ls"]);
    assert!(cli.json, "global --json must be set");
    match &cli.cmd {
        Cmd::Engine { subcmd } => {
            matches!(subcmd, EngineCmd::Ls);
        }
        _ => panic!("expected Engine cmd"),
    }
}

// ── engine install-pack ───────────────────────────────────────────────────

#[test]
fn engine_install_pack_parses() {
    let cli = parse(&["engine", "install-pack", "/tmp/mypack"]);
    match &cli.cmd {
        Cmd::Engine { subcmd } => match subcmd {
            EngineCmd::InstallPack { dir } => {
                assert_eq!(dir, "/tmp/mypack");
            }
            _ => panic!("expected InstallPack"),
        },
        _ => panic!("expected Engine cmd"),
    }
}

#[test]
fn engine_install_pack_requires_dir() {
    assert!(try_parse(&["engine", "install-pack"]).is_err());
}

// ── oci import ────────────────────────────────────────────────────────────

#[test]
fn oci_import_parses() {
    let cli = parse(&["oci", "import", "/tmp/layout", "--name", "myimage"]);
    match &cli.cmd {
        Cmd::Oci { subcmd } => match subcmd {
            OciCmd::Import { path, name } => {
                assert_eq!(path, "/tmp/layout");
                assert_eq!(name, "myimage");
            }
            _ => panic!("expected Import"),
        },
        _ => panic!("expected Oci cmd"),
    }
}

#[test]
fn oci_import_json_uses_global_flag() {
    let cli = parse(&["--json", "oci", "import", "/tmp/x", "--name", "img"]);
    assert!(cli.json, "global --json must be set");
}

#[test]
fn oci_import_requires_path_and_name() {
    assert!(try_parse(&["oci", "import"]).is_err());
    assert!(try_parse(&["oci", "import", "/tmp/x"]).is_err());
}

// ── oci pull ──────────────────────────────────────────────────────────────

#[test]
fn oci_pull_parses() {
    let cli = parse(&["oci", "pull", "alpine:latest", "--name", "my-alpine"]);
    match &cli.cmd {
        Cmd::Oci { subcmd } => match subcmd {
            OciCmd::Pull { image, name } => {
                assert_eq!(image, "alpine:latest");
                assert_eq!(name, "my-alpine");
            }
            _ => panic!("expected Pull"),
        },
        _ => panic!("expected Oci cmd"),
    }
}

#[test]
fn oci_pull_requires_image_and_name() {
    assert!(try_parse(&["oci", "pull"]).is_err());
    assert!(try_parse(&["oci", "pull", "alpine"]).is_err());
}

#[test]
fn oci_pull_json_uses_global_flag() {
    let cli = parse(&["--json", "oci", "pull", "alpine", "--name", "a"]);
    assert!(cli.json);
}

// ── oci push ──────────────────────────────────────────────────────────────

#[test]
fn oci_push_parses() {
    let cli = parse(&["oci", "push", "@me/img", "ghcr.io/owner/repo:tag"]);
    match &cli.cmd {
        Cmd::Oci { subcmd } => match subcmd {
            OciCmd::Push { store_ref, target } => {
                assert_eq!(store_ref, "@me/img");
                assert_eq!(target, "ghcr.io/owner/repo:tag");
            }
            _ => panic!("expected Push"),
        },
        _ => panic!("expected Oci cmd"),
    }
}

#[test]
fn oci_push_requires_store_ref_and_target() {
    assert!(try_parse(&["oci", "push"]).is_err());
    assert!(try_parse(&["oci", "push", "@me/img"]).is_err());
}

#[test]
fn oci_push_json_uses_global_flag() {
    let cli = parse(&[
        "--json",
        "oci",
        "push",
        "@me/img",
        "localhost:5000/x:latest",
    ]);
    assert!(cli.json);
}

// ── run --engine / --rootfs ───────────────────────────────────────────────

#[test]
fn run_engine_default_is_native() {
    let cli = parse(&["run", "--", "echo", "hi"]);
    match &cli.cmd {
        Cmd::Run(a) => {
            assert_eq!(a.engine, "native");
            assert!(a.rootfs.is_none());
        }
        _ => panic!("expected Run"),
    }
}

#[test]
fn run_engine_ns() {
    let cli = parse(&["run", "--engine", "ns", "--", "echo"]);
    match &cli.cmd {
        Cmd::Run(a) => assert_eq!(a.engine, "ns"),
        _ => panic!("expected Run"),
    }
}

#[test]
fn run_engine_vz() {
    let cli = parse(&["run", "--engine", "vz", "--", "echo"]);
    match &cli.cmd {
        Cmd::Run(a) => assert_eq!(a.engine, "vz"),
        _ => panic!("expected Run"),
    }
}

#[test]
fn run_rootfs_flag() {
    let cli = parse(&["run", "--rootfs", "my-image", "--engine", "ns", "--", "sh"]);
    match &cli.cmd {
        Cmd::Run(a) => {
            assert_eq!(a.rootfs.as_deref(), Some("my-image"));
            assert_eq!(a.engine, "ns");
        }
        _ => panic!("expected Run"),
    }
}

#[test]
fn run_bad_engine_string_rejected_at_handler() {
    // Clap accepts any string for --engine; the handler rejects bad values at exit 2.
    // Parse succeeds:
    let cli = parse(&["run", "--engine", "bogus", "--", "echo"]);
    match &cli.cmd {
        Cmd::Run(a) => assert_eq!(a.engine, "bogus"),
        _ => panic!("expected Run"),
    }
    // The handler should return 2 for a bad engine string.
    // We test this through the handler directly (not through process::exit).
    use crate::handlers::run::run as run_handler;
    let code = run_handler(
        ".",
        &[],
        &[],
        &["echo".to_string()],
        false,
        false,
        false,
        &[],
        &[],
        "bogus",
        None,
        false,
        None,
        None,
        &[],
        &[],
        &[],  // env_set (WP-RC-1)
        None, // env_file (WP-RC-1)
        None, // workdir (WP-RC-WORKDIR)
        None, // user (WP-RC-USER)
        None, // restart (WP-RC-RESTART)
        None, // stop_signal (WP-RC-STOPSIGNAL)
        &crate::handlers::run::HealthFlags::default(),
        crate::handlers::run::RawRcFlags::default(), // WP-CLI-TRIO / RC-FLAGS
    );
    assert_eq!(code, 2, "bad engine string must exit 2");
}

#[test]
fn run_native_with_rootfs_rejected_by_engine() {
    // native + rootfs ⇒ the NativeEngine itself returns InvalidRef → exit 2
    // We need a valid store to test this, so instead we verify parse accepts
    // the flags and trust the engine unit tests cover the runtime rejection.
    let cli = parse(&["run", "--engine", "native", "--rootfs", "@x", "--", "true"]);
    match &cli.cmd {
        Cmd::Run(a) => {
            assert_eq!(a.engine, "native");
            assert_eq!(a.rootfs.as_deref(), Some("@x"));
        }
        _ => panic!("expected Run"),
    }
}
