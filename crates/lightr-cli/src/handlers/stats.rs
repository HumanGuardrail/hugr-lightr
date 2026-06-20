//! `lightr stats` handler — live resource-usage stats (docker stats), one-shot.
//!
//! macOS/native is PROCESS-based: there is NO cgroup, so MEM LIMIT / cgroup
//! quotas are honestly N/A. We report the supervisor process's CPU%/MEM%/RSS
//! from `ps(1)` — a Docker-stats-like one-shot table (the `--no-stream` shape),
//! never a fabricated limit (tense-law).
//!
//! Columns: CONTAINER ID · NAME · CPU % · MEM % · MEM USAGE (RSS). With no
//! target, every RUNNING run is listed. A stopped run reports `0.00% / 0.00% /
//! —` for its row; a named-but-unknown target is "No such container" (exit 1,
//! Docker `docker stats <missing>` parity).

use lightr_run::{ps, resolve};

use crate::handlers::runproc::{fmt_rss, pid_for_id, query_stats, short_id, ProcStat};
use crate::lightr_home;

const HEADER: &str = "CONTAINER ID  NAME              CPU %     MEM %     MEM USAGE";

/// Print one stats row. A live sample renders real numbers; a stopped run (no
/// live process) renders the honest `0.00% / 0.00% / —` resting row.
fn print_row(id: &str, name: &str, sample: Option<&ProcStat>) {
    let sid = short_id(id);
    // A live sample renders real numbers; a stopped run (no live process)
    // renders the honest resting row (`0.00% / 0.00% / —`, an em-dash usage —
    // there is no native cgroup, so no value to report at rest).
    let (cpu, mem, usage) = match sample {
        Some(s) => (
            format!("{:.2}%", s.cpu_pct),
            format!("{:.2}%", s.mem_pct),
            fmt_rss(s.rss_kb),
        ),
        None => (
            "0.00%".to_string(),
            "0.00%".to_string(),
            "\u{2014}".to_string(),
        ),
    };
    println!("{sid:<13} {name:<17} {cpu:<9} {mem:<9} {usage}");
}

/// Resolve + sample one target. Returns the exit code; prints the table.
fn stats_one(home: &std::path::Path, target: &str) -> i32 {
    // ref → id. Docker `stats <missing>` → "No such container" (exit 1).
    let id = match resolve(home, target) {
        Ok(id) => id,
        Err(_) => {
            eprintln!("lightr: No such container: {target}");
            return 1;
        }
    };

    println!("{HEADER}");
    // A live supervisor pid + a successful ps sample ⇒ real numbers; otherwise
    // the honest resting row (stopped / reaped).
    let sample = pid_for_id(home, &id).and_then(query_stats);
    print_row(&id, &id, sample.as_ref());
    0
}

/// Sample ALL running runs (no target). Honest empty table when nothing runs.
fn stats_all(home: &std::path::Path) -> i32 {
    let runs = match ps(home) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lightr: {e}");
            return 1;
        }
    };

    println!("{HEADER}");
    for r in runs.iter().filter(|r| r.running) {
        let sample = pid_for_id(home, &r.id).and_then(query_stats);
        print_row(&r.id, &r.id, sample.as_ref());
    }
    0
}

pub fn run(target: Option<&str>) -> i32 {
    let home = lightr_home();
    match target {
        Some(t) => stats_one(&home, t),
        None => stats_all(&home),
    }
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
