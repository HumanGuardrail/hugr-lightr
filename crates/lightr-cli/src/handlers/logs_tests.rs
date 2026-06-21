//! Tests for the `lightr logs` handler — split out via `#[path]` to keep
//! logs.rs under the 400-line godfile cap (house convention).
//!
//! WP-LIFE-LOGS adds `--tail N`, `-f/--follow`, `--since`, `-t/--timestamps`.
//! The pure selection/streaming helpers (`select_tail`, `bytes_after`,
//! `since_excludes_all`, `parse_since`) carry the load and touch NO
//! process-global state, so they are trivially parallel-safe. The exit-code
//! contract (unknown id ⇒ 2; behavior-preserved no-flag dump ⇒ 0) is exercised
//! end-to-end under the crate-wide `ENV_LOCK` while `LIGHTR_HOME` is set, since
//! `lightr_home()` reads a process-global env var (same pattern as inspect).

use std::fs;

use super::{bytes_after, parse_since, select_tail, since_excludes_all};
use super::{run as logs_run, LogOpts};
use crate::test_lock::ENV_LOCK;
use lightr_run::LogStream;

// ── select_tail (the --tail N core) ─────────────────────────────────────────

#[test]
fn tail_none_returns_all() {
    let data = b"a\nb\nc\n";
    assert_eq!(select_tail(data, None), data);
}

#[test]
fn tail_last_n_lines() {
    let data = b"l1\nl2\nl3\nl4\nl5\n";
    // Last 2 lines, terminators preserved.
    assert_eq!(select_tail(data, Some(2)), b"l4\nl5\n");
}

#[test]
fn tail_n_larger_than_lines_returns_all() {
    let data = b"only\ntwo\n";
    assert_eq!(select_tail(data, Some(99)), data);
}

#[test]
fn tail_zero_returns_empty() {
    let data = b"a\nb\n";
    assert_eq!(select_tail(data, Some(0)), b"");
}

#[test]
fn tail_no_trailing_newline() {
    let data = b"first\nsecond"; // last line unterminated
    assert_eq!(select_tail(data, Some(1)), b"second");
}

#[test]
fn tail_single_line_no_newline() {
    let data = b"solo";
    assert_eq!(select_tail(data, Some(1)), b"solo");
    assert_eq!(select_tail(data, Some(5)), b"solo");
}

#[test]
fn tail_empty_input() {
    assert_eq!(select_tail(b"", Some(3)), b"");
    assert_eq!(select_tail(b"", None), b"");
}

// ── bytes_after (the --follow append core) ──────────────────────────────────

#[test]
fn follow_streams_appends_then_no_more() {
    // Parallel-safe: unique tempdir per test, no shared/global state.
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("stdout.log");

    fs::write(&log, b"first\n").unwrap();
    let (chunk1, off1) = bytes_after(&log, 0).unwrap();
    assert_eq!(chunk1, b"first\n");
    assert_eq!(off1, 6);

    // No new bytes ⇒ empty, offset unchanged (the loop's "no progress" signal).
    let (chunk_none, off_same) = bytes_after(&log, off1).unwrap();
    assert!(chunk_none.is_empty());
    assert_eq!(off_same, off1);

    // Append more ⇒ only the new bytes stream.
    fs::write(&log, b"first\nsecond\n").unwrap();
    let (chunk2, off2) = bytes_after(&log, off1).unwrap();
    assert_eq!(chunk2, b"second\n");
    assert_eq!(off2, 13);
}

#[test]
fn follow_missing_file_is_empty_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("stderr.log");
    let (chunk, off) = bytes_after(&missing, 0).unwrap();
    assert!(chunk.is_empty());
    assert_eq!(off, 0);
}

#[test]
fn follow_offset_past_eof_clamps() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("stdout.log");
    fs::write(&log, b"abc").unwrap();
    // Offset beyond EOF (e.g. file truncated) ⇒ no panic, empty slice.
    let (chunk, off) = bytes_after(&log, 999).unwrap();
    assert!(chunk.is_empty());
    assert_eq!(off, 3);
}

// ── --since honest semantics ────────────────────────────────────────────────

#[test]
fn parse_since_unix_seconds() {
    assert_eq!(parse_since("1717600000"), Some(1_717_600_000));
    assert_eq!(parse_since("  42 "), Some(42));
    assert_eq!(parse_since("not-a-ts"), None);
    assert_eq!(parse_since("2026-06-19T00:00:00Z"), None); // lenient include
}

#[test]
fn since_excludes_old_file_includes_recent() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("stdout.log");
    fs::write(&log, b"line\n").unwrap();

    // Cutoff far in the future ⇒ file mtime is older ⇒ exclude all.
    let far_future = "9999999999";
    assert!(since_excludes_all(
        tmp.path(),
        &LogStream::Stdout,
        Some(far_future)
    ));

    // Cutoff at epoch ⇒ file is newer ⇒ include.
    assert!(!since_excludes_all(
        tmp.path(),
        &LogStream::Stdout,
        Some("0")
    ));

    // No --since ⇒ never excludes.
    assert!(!since_excludes_all(tmp.path(), &LogStream::Stdout, None));

    // Unparseable --since ⇒ lenient include (don't exclude).
    assert!(!since_excludes_all(
        tmp.path(),
        &LogStream::Stdout,
        Some("yesterday")
    ));
}

// ── exit-code contract (end-to-end, under ENV_LOCK) ─────────────────────────

fn base_opts() -> LogOpts<'static> {
    LogOpts {
        stderr: false,
        both: false,
        follow: false,
        tail: None,
        since: None,
        timestamps: false,
    }
}

#[test]
fn unknown_run_id_exits_1() {
    // Docker parity (WP-EXIT-CODE): `logs <missing>` → "No such container",
    // exit 1 (a missing container is NOT a usage error).
    let tmp = tempfile::tempdir().unwrap();
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: single-threaded under ENV_LOCK.
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = logs_run("does-not-exist", &base_opts());
    unsafe { std::env::remove_var("LIGHTR_HOME") };
    assert_eq!(code, 1);
}

#[test]
fn no_flags_dumps_full_log_exit_0() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run").join("r1");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("stdout.log"), b"hello world\n").unwrap();
    // Mark exited so the (non-follow) base path returns immediately.
    fs::write(run_dir.join("status"), b"exited 0\n").unwrap();

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = logs_run("r1", &base_opts()); // behavior-preserved path
    unsafe { std::env::remove_var("LIGHTR_HOME") };
    assert_eq!(code, 0);
}

#[test]
fn tail_path_exit_0() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run").join("r2");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("stdout.log"), b"a\nb\nc\n").unwrap();

    let mut opts = base_opts();
    opts.tail = Some(2);

    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    unsafe { std::env::set_var("LIGHTR_HOME", tmp.path()) };
    let code = logs_run("r2", &opts);
    unsafe { std::env::remove_var("LIGHTR_HOME") };
    assert_eq!(code, 0);
}
