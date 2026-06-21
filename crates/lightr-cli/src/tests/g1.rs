use super::*;

// ── snapshot ──────────────────────────────────────────────────────────

#[test]
fn snapshot_minimal() {
    let cli = parse(&["snapshot", "--name", "myref"]);
    match cli.cmd {
        Cmd::Snapshot { dir, name } => {
            assert_eq!(dir, ".");
            assert_eq!(name, "myref");
        }
        _ => panic!("wrong cmd"),
    }
    assert!(!cli.json);
    assert!(!cli.explain);
}

#[test]
fn snapshot_all_flags() {
    let cli = parse(&[
        "--json",
        "--explain",
        "snapshot",
        "--dir",
        "/tmp/x",
        "--name",
        "v1",
    ]);
    match cli.cmd {
        Cmd::Snapshot { dir, name } => {
            assert_eq!(dir, "/tmp/x");
            assert_eq!(name, "v1");
        }
        _ => panic!("wrong cmd"),
    }
    assert!(cli.json);
    assert!(cli.explain);
}

#[test]
fn snapshot_requires_name() {
    assert!(try_parse(&["snapshot"]).is_err());
}

// ── hydrate ───────────────────────────────────────────────────────────

#[test]
fn hydrate_minimal() {
    let cli = parse(&["hydrate", "/dest", "--name", "v1"]);
    match cli.cmd {
        Cmd::Hydrate { dest, name, verify } => {
            assert_eq!(dest, "/dest");
            assert_eq!(name, "v1");
            assert!(!verify);
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn hydrate_requires_dest_and_name() {
    assert!(try_parse(&["hydrate", "--name", "v1"]).is_err());
    assert!(try_parse(&["hydrate", "/dest"]).is_err());
}

#[test]
fn hydrate_json_flag() {
    let cli = parse(&["--json", "hydrate", "/d", "--name", "r"]);
    assert!(cli.json);
}

// ── status ────────────────────────────────────────────────────────────

#[test]
fn status_minimal() {
    let cli = parse(&["status", "--name", "myref"]);
    match cli.cmd {
        Cmd::Status { dir, name } => {
            assert_eq!(dir, ".");
            assert_eq!(name, "myref");
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn status_with_dir() {
    let cli = parse(&["status", "--dir", "/src", "--name", "r"]);
    match cli.cmd {
        Cmd::Status { dir, name } => {
            assert_eq!(dir, "/src");
            assert_eq!(name, "r");
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn status_requires_name() {
    assert!(try_parse(&["status"]).is_err());
}

// ── run ───────────────────────────────────────────────────────────────

#[test]
fn run_minimal() {
    let cli = parse(&["run", "--", "echo", "hello"]);
    match &cli.cmd {
        Cmd::Run(a) => {
            assert_eq!(a.dir, ".");
            assert!(a.input.is_empty());
            assert!(a.env.is_empty());
            assert_eq!(a.command, &["echo", "hello"]);
            assert!(!a.detach);
            assert!(a.mount.is_empty());
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn run_all_flags() {
    let cli = parse(&[
        "run", "--dir", "/work", "--input", "/a", "--input", "/b", "--env", "FOO", "--env", "BAR",
        "--", "make", "all",
    ]);
    match &cli.cmd {
        Cmd::Run(a) => {
            assert_eq!(a.dir, "/work");
            assert_eq!(a.input, &["/a", "/b"]);
            assert_eq!(a.env, &["FOO", "BAR"]);
            assert_eq!(a.command, &["make", "all"]);
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn run_requires_command() {
    assert!(try_parse(&["run"]).is_err());
}

#[test]
fn run_detach_flag() {
    let cli = parse(&["run", "-d", "--", "sleep", "100"]);
    match &cli.cmd {
        Cmd::Run(a) => {
            assert!(a.detach, "expected detach to be true");
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn run_mount_single() {
    let cli = parse(&["run", "--mount", "myref:subdir", "--", "echo"]);
    match &cli.cmd {
        Cmd::Run(a) => {
            assert_eq!(a.mount, &["myref:subdir"]);
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn run_mount_multiple() {
    let cli = parse(&["run", "--mount", "r1:a", "--mount", "r2:b", "--", "cmd"]);
    match &cli.cmd {
        Cmd::Run(a) => {
            assert_eq!(a.mount.len(), 2);
            assert_eq!(a.mount[0], "r1:a");
            assert_eq!(a.mount[1], "r2:b");
        }
        _ => panic!("wrong cmd"),
    }
}
