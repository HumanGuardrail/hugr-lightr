//! Shared process-introspection helpers for `lightr stats` + `lightr top`.
//!
//! macOS/native is PROCESS-based (no cgroup, no PID namespace), so these are
//! HONEST process-level views of a detached run's supervisor process tree —
//! Docker-faithful columns where the platform allows, honest about the fields a
//! cgroup would carry but a bare process cannot (e.g. MEM LIMIT). Numbers come
//! straight from `ps(1)`; nothing is fabricated (tense-law).
//!
//! The pid lives in `<home>/run/<id>/pid` — the same file `lightr-run`'s ps path
//! writes/reads (`run/ps.rs` → `read_pid_file`). `lightr_run::ps()` does not
//! expose the pid, so we replicate that read here rather than reach into
//! lightr-run internals.

use std::path::Path;

/// Read the supervisor pid for a resolved run id from `<home>/run/<id>/pid`.
///
/// Returns `None` when the run dir / pid file is absent or unparseable (a
/// never-detached or already-reaped run) — the caller renders that honestly as
/// "not running", never as a fabricated metric.
pub fn pid_for_id(home: &Path, id: &str) -> Option<i32> {
    let pid_path = home.join("run").join(id).join("pid");
    std::fs::read_to_string(pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

/// One process's resource sample (`ps -o pid,pcpu,pmem,rss,comm`).
///
/// `rss_kb` is resident set size in KiB exactly as `ps` reports it (no cgroup
/// limit exists natively, so there is no usage/limit ratio to compute).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcStat {
    pub pid: i32,
    pub cpu_pct: f64,
    pub mem_pct: f64,
    pub rss_kb: u64,
    pub comm: String,
}

/// One `ps -ef`-style row for `lightr top` (`ps -o pid,user,time,command`).
#[derive(Debug, Clone, PartialEq)]
pub struct TopRow {
    pub pid: i32,
    pub user: String,
    pub time: String,
    pub command: String,
}

// ── ps(1) shell-outs (unix) ────────────────────────────────────────────────

/// Query CPU%/MEM%/RSS/comm for one pid via `ps`. `None` if the process is gone
/// or `ps` produced no data row (honest "not running"/"—", never a fake zero).
#[cfg(unix)]
pub fn query_stats(pid: i32) -> Option<ProcStat> {
    let out = std::process::Command::new("ps")
        .args(["-o", "pid,pcpu,pmem,rss,comm", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_stats_line(text.lines().nth(1)?)
}

/// Query `ps -ef`-style rows for a pid and (optionally) its direct children, for
/// `lightr top`. The supervisor pid is always first; children (via `pgrep -P`)
/// follow when discoverable. Empty vec ⇒ the process is gone.
#[cfg(unix)]
pub fn query_top(pid: i32) -> Vec<TopRow> {
    let mut pids = vec![pid];
    if let Ok(out) = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Ok(child) = line.trim().parse::<i32>() {
                    pids.push(child);
                }
            }
        }
    }

    let pid_args = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let out = match std::process::Command::new("ps")
        .args(["-o", "pid,user,time,command", "-p", &pid_args])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().skip(1).filter_map(parse_top_line).collect()
}

// WIN-PATH: Windows has no `ps`/`pgrep` and the native engine does not run there
// (the daemonless core targets unix; the windows CI gate is build+clippy only).
// Fail closed with empty data — the caller renders an honest "not running"-class
// view rather than fabricating metrics. Runtime-validatable only on Windows.
#[cfg(not(unix))]
pub fn query_stats(_pid: i32) -> Option<ProcStat> {
    None
}

#[cfg(not(unix))]
pub fn query_top(_pid: i32) -> Vec<TopRow> {
    Vec::new()
}

// ── pure parsers (cross-platform, directly tested) ─────────────────────────

/// Parse a `ps -o pid,pcpu,pmem,rss,comm` data line. Whitespace-split on the
/// first four columns; `comm` is the remainder (a path may contain spaces only
/// in pathological cases — `comm` is the basename/exec path, joined verbatim).
pub fn parse_stats_line(line: &str) -> Option<ProcStat> {
    let mut it = line.split_whitespace();
    let pid = it.next()?.parse::<i32>().ok()?;
    let cpu_pct = parse_locale_f64(it.next()?)?;
    let mem_pct = parse_locale_f64(it.next()?)?;
    let rss_kb = it.next()?.parse::<u64>().ok()?;
    let comm: String = it.collect::<Vec<_>>().join(" ");
    Some(ProcStat {
        pid,
        cpu_pct,
        mem_pct,
        rss_kb,
        comm,
    })
}

/// Parse a `ps -o pid,user,time,command` data line. First three columns are
/// fixed; `command` (which contains spaces) is the remainder.
pub fn parse_top_line(line: &str) -> Option<TopRow> {
    let mut it = line.split_whitespace();
    let pid = it.next()?.parse::<i32>().ok()?;
    let user = it.next()?.to_string();
    let time = it.next()?.to_string();
    let command: String = it.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return None;
    }
    Some(TopRow {
        pid,
        user,
        time,
        command,
    })
}

/// `ps` formats floats per the C locale of the host, which on some systems uses
/// a comma decimal separator (`0,0`). Normalize before parsing so we never drop
/// a valid sample to a parse error.
fn parse_locale_f64(s: &str) -> Option<f64> {
    s.replace(',', ".").parse::<f64>().ok()
}

/// Render KiB as a Docker-stats-like human size (`12.3MiB`). RSS only — there is
/// no native cgroup limit, so this is a single value, never a `used / limit`.
pub fn fmt_rss(rss_kb: u64) -> String {
    let kb = rss_kb as f64;
    if kb >= 1024.0 * 1024.0 {
        format!("{:.2}GiB", kb / (1024.0 * 1024.0))
    } else if kb >= 1024.0 {
        format!("{:.2}MiB", kb / 1024.0)
    } else {
        format!("{kb:.0}KiB")
    }
}

/// Short id for the CONTAINER ID column (Docker shows 12 chars). Our ids are
/// `<nanos>-<pid>`; truncate to 12 for display parity.
pub fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

#[cfg(test)]
#[path = "runproc_tests.rs"]
mod tests;
