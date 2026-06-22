//! FIX #74 — tests for the `docker run` shim flag parser. These prove every
//! documented flag is FORWARDED (not silent-dropped), `--flag=value` joins, the
//! image/command split, and that unrecognized / grammar-mismatch flags are
//! HONEST errors rather than silent no-ops.

use super::*;

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn forwards_core_run_flags() {
    // `docker run -e K=V -p 8080:80 --name foo -v /h:/c -w /app -u 1000 img cmd`
    let parsed = parse(&argv(&[
        "-e", "K=V", "-p", "8080:80", "--name", "foo", "-v", "/h:/c", "-w", "/app", "-u", "1000",
        "img", "cmd", "arg",
    ]))
    .expect("valid flags must parse");

    assert_eq!(parsed.env_set, vec!["K=V"]);
    assert_eq!(parsed.publish, vec!["8080:80"]);
    assert_eq!(parsed.runflags.name.as_deref(), Some("foo"));
    assert_eq!(parsed.runflags.volume, vec!["/h:/c"]);
    assert_eq!(parsed.workdir.as_deref(), Some("/app"));
    assert_eq!(parsed.user.as_deref(), Some("1000"));
    assert_eq!(parsed.image.as_deref(), Some("img"));
    assert_eq!(parsed.command, vec!["cmd", "arg"]);
}

#[test]
fn forwards_lifecycle_and_resource_flags() {
    let parsed = parse(&argv(&[
        "--rm",
        "-d",
        "--restart",
        "always",
        "--stop-signal",
        "SIGINT",
        "--memory",
        "512m",
        "--cpus",
        "2",
        "--entrypoint",
        "/bin/sh",
        "--label",
        "a=b",
        "-P",
        "img",
    ]))
    .expect("valid flags must parse");

    assert!(parsed.runflags.rm);
    assert!(parsed.detach);
    assert!(parsed.publish_all);
    assert_eq!(parsed.restart.as_deref(), Some("always"));
    assert_eq!(parsed.stop_signal.as_deref(), Some("SIGINT"));
    assert_eq!(parsed.memory.as_deref(), Some("512m"));
    assert_eq!(parsed.cpus.as_deref(), Some("2"));
    assert_eq!(parsed.runflags.entrypoint.as_deref(), Some("/bin/sh"));
    assert_eq!(parsed.rc.label, vec!["a=b"]);
    assert_eq!(parsed.image.as_deref(), Some("img"));
}

#[test]
fn env_file_and_inline_equals_form() {
    // `--flag=value` joined form must parse identically to the split form.
    let parsed = parse(&argv(&[
        "--env-file=.env",
        "--name=bar",
        "-e",
        "X=1",
        "img",
    ]))
    .expect("parses");
    assert_eq!(parsed.env_file.as_deref(), Some(".env"));
    assert_eq!(parsed.runflags.name.as_deref(), Some("bar"));
    assert_eq!(parsed.env_set, vec!["X=1"]);
}

#[test]
fn flags_after_image_are_command_not_flags() {
    // docker stops flag parsing at the first positional: `-x` after the image is
    // part of the command, never re-interpreted as a shim flag.
    let parsed = parse(&argv(&["img", "ls", "-la"])).expect("parses");
    assert_eq!(parsed.image.as_deref(), Some("img"));
    assert_eq!(parsed.command, vec!["ls", "-la"]);
}

// `DockerRunArgs` deliberately does NOT derive `PartialEq`/`Debug` (its
// `RawRcFlags`/`RawRunFlags` members do not), so error tests assert on the
// `i32` exit code via `.err()` rather than comparing the whole `Result`.

#[test]
fn unrecognized_flag_is_honest_error_not_silent() {
    // The cardinal rule: an unknown flag is exit 2, NEVER swallowed.
    assert_eq!(parse(&argv(&["--frobnicate", "img"])).err(), Some(2));
}

#[test]
fn grammar_mismatch_flags_are_honest_errors() {
    // native `--mount`/`--secret`/`--config` use a different value grammar, so a
    // raw forward would misparse — honest error instead of a silent misforward.
    assert_eq!(
        parse(&argv(&["--mount", "type=bind,src=/a,dst=/b", "img"])).err(),
        Some(2)
    );
    assert_eq!(
        parse(&argv(&["--secret", "id=s,src=/a", "img"])).err(),
        Some(2)
    );
    assert_eq!(parse(&argv(&["--config", "x", "img"])).err(), Some(2));
}

#[test]
fn flag_missing_value_is_honest_error() {
    // `-e` with no following value ⇒ exit 2 (never a half-parsed forward).
    assert_eq!(parse(&argv(&["-e"])).err(), Some(2));
}
