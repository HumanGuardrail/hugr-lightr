//! Tests for the shared process-introspection helpers. Pure logic + the
//! pid-file read are exercised here (no process-global mutation, tempdir-scoped,
//! parallel-safe). The live `ps`/`pgrep` shell-outs are exercised end-to-end by
//! the stats/top handler tests against a real short-lived child.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_tmp() -> tempfile::TempDir {
    // Atomic-counter + nanos keeps each test's home disjoint (parallel-safe).
    let _ = COUNTER.fetch_add(1, Ordering::Relaxed);
    tempfile::tempdir().unwrap()
}

// ── pid_for_id ──────────────────────────────────────────────────────────────

#[test]
fn pid_for_id_reads_the_pid_file() {
    let tmp = unique_tmp();
    let id = "1717600000000000000-42";
    let run_dir = tmp.path().join("run").join(id);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("pid"), "12345\n").unwrap();

    assert_eq!(pid_for_id(tmp.path(), id), Some(12345));
}

#[test]
fn pid_for_id_absent_is_none() {
    let tmp = unique_tmp();
    assert_eq!(pid_for_id(tmp.path(), "no-such-id"), None);
}

#[test]
fn pid_for_id_garbage_is_none() {
    let tmp = unique_tmp();
    let id = "garbage-1";
    let run_dir = tmp.path().join("run").join(id);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("pid"), "not-a-number").unwrap();

    assert_eq!(pid_for_id(tmp.path(), id), None);
}

// ── parse_stats_line ──────────────────────────────────────────────────────

#[test]
fn parse_stats_line_dot_locale() {
    let s = parse_stats_line("  321  1.5  2.0  20480 /bin/sleep").unwrap();
    assert_eq!(s.pid, 321);
    assert_eq!(s.cpu_pct, 1.5);
    assert_eq!(s.mem_pct, 2.0);
    assert_eq!(s.rss_kb, 20480);
    assert_eq!(s.comm, "/bin/sleep");
}

#[test]
fn parse_stats_line_comma_locale() {
    // Some hosts' C locale uses a comma decimal separator.
    let s = parse_stats_line("19939   0,0  0,0   1804 /bin/zsh").unwrap();
    assert_eq!(s.cpu_pct, 0.0);
    assert_eq!(s.mem_pct, 0.0);
    assert_eq!(s.rss_kb, 1804);
    assert_eq!(s.comm, "/bin/zsh");
}

#[test]
fn parse_stats_line_garbage_is_none() {
    assert!(parse_stats_line("this is not a ps row").is_none());
    assert!(parse_stats_line("").is_none());
}

// ── parse_top_line ────────────────────────────────────────────────────────

#[test]
fn parse_top_line_keeps_full_command() {
    let r = parse_top_line("  321 root    0:00.01 /bin/sleep 3600").unwrap();
    assert_eq!(r.pid, 321);
    assert_eq!(r.user, "root");
    assert_eq!(r.time, "0:00.01");
    assert_eq!(r.command, "/bin/sleep 3600");
}

#[test]
fn parse_top_line_no_command_is_none() {
    // Only pid/user/time, no command → not a real row.
    assert!(parse_top_line("321 root 0:00").is_none());
}

// ── fmt_rss ───────────────────────────────────────────────────────────────

#[test]
fn fmt_rss_scales() {
    assert_eq!(fmt_rss(512), "512KiB");
    assert_eq!(fmt_rss(2048), "2.00MiB");
    assert_eq!(fmt_rss(3 * 1024 * 1024), "3.00GiB");
}

// ── short_id ──────────────────────────────────────────────────────────────

#[test]
fn short_id_truncates_to_twelve() {
    assert_eq!(short_id("1717600000000000000-42").len(), 12);
    assert_eq!(short_id("abc"), "abc");
}
