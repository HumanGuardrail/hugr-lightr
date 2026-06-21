//! `lightr logs` handler — read/stream a run's stdout/stderr log files.
//!
//! Docker-parity flags (WP-LIFE-LOGS): `--tail N`, `-f/--follow`,
//! `--since <ts>`, `-t/--timestamps`. The base contract (no flags ⇒ full dump)
//! is byte-for-byte preserved.
//!
//! Honesty note: lightr's detached runs write RAW stdout/stderr to
//! `stdout.log`/`stderr.log` — the on-disk format carries NO per-line
//! timestamps. So `-t/--timestamps` and `--since` cannot synthesize a per-line
//! clock from nothing; rather than fabricate one (tense-law), they fall back to
//! the file's last-modified time as a single honest signal and say so on stderr.

use std::io::Write;
use std::path::Path;

use lightr_run::{logs, run_status, LogStream, RunStatus};

use crate::{exit::die_lightr, lightr_home};

/// Bundle of the WP-LIFE-LOGS flags, threaded from dispatch.
pub struct LogOpts<'a> {
    pub stderr: bool,
    pub both: bool,
    pub follow: bool,
    /// `--tail N` — print only the last N lines. `None` ⇒ all (our base
    /// contract; a literal `--tail all` is parsed to `None` upstream so the
    /// no-flag full-dump behavior is preserved).
    pub tail: Option<usize>,
    /// `--since <ts>` — raw timestamp string (unix seconds; RFC3339 lenient).
    pub since: Option<&'a str>,
    /// `-t/--timestamps` — surface the (file-level) timestamp signal.
    pub timestamps: bool,
}

pub fn run(id: &str, opts: &LogOpts) -> i32 {
    let home = lightr_home();
    let run_dir = home.join("run").join(id);

    if !run_dir.exists() {
        eprintln!("lightr: unknown run id");
        return 2;
    }

    let stream = if opts.both {
        LogStream::Both
    } else if opts.stderr {
        LogStream::Stderr
    } else {
        LogStream::Stdout
    };

    // Honest disclosure: the on-disk log format has no per-line timestamps, so
    // `--since`/`-t` cannot operate per-line. We surface a single file-level
    // mtime and say so, rather than fabricate a clock (tense-law).
    let enrich = opts.timestamps || opts.since.is_some();

    // Fast path: no tail, no enrichment, no follow ⇒ delegate to the frozen
    // lightr-run reader so the base dump is byte-for-byte preserved.
    if opts.tail.is_none() && !enrich && !opts.follow {
        return match logs(&run_dir, stream, false) {
            Ok(()) => 0,
            Err(e) => die_lightr(&e),
        };
    }

    if enrich {
        emit_timestamp_note(&run_dir, &stream, opts.since);
        if since_excludes_all(&run_dir, &stream, opts.since) {
            return 0;
        }
    }

    let paths = stream_paths(&run_dir, &stream);

    // Print the (optionally tail-limited) existing content first.
    for p in &paths {
        if let Err(e) = print_tail(p, opts.tail) {
            return die_lightr(&e);
        }
    }

    if !opts.follow {
        return 0;
    }

    // Bounded follow: poll for appends, stop when the run has exited and the
    // streams are drained, OR when a hard cap is hit (never hang forever —
    // no-daemon discipline: nothing of ours should spin unbounded).
    follow_bounded(id, &home, &paths)
}

/// Resolve the concrete log file path(s) for the selected stream.
fn stream_paths(run_dir: &Path, stream: &LogStream) -> Vec<std::path::PathBuf> {
    let out = run_dir.join("stdout.log");
    let err = run_dir.join("stderr.log");
    match stream {
        LogStream::Stdout => vec![out],
        LogStream::Stderr => vec![err],
        LogStream::Both => vec![out, err],
    }
}

/// Print the last `tail` lines of `path` (or all when `tail` is `None`).
/// Missing file ⇒ nothing (a stream may have produced no output).
fn print_tail(path: &Path, tail: Option<usize>) -> lightr_core::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let data = std::fs::read(path).map_err(lightr_core::LightrError::Io)?;
    let selected = select_tail(&data, tail);
    let mut out = std::io::stdout();
    out.write_all(selected)
        .map_err(lightr_core::LightrError::Io)?;
    out.flush().map_err(lightr_core::LightrError::Io)?;
    Ok(())
}

/// Pure tail selection: the last `tail` lines of `data` (or all when `None`),
/// returned as a byte slice into `data`. Line terminators are preserved; a
/// single trailing empty segment after the final '\n' is not over-counted.
fn select_tail(data: &[u8], tail: Option<usize>) -> &[u8] {
    let Some(n) = tail else { return data };
    if n == 0 {
        return &data[data.len()..];
    }
    // Walk backwards counting '\n' boundaries. We want the byte offset just
    // after the (n)-th-from-last line start. A trailing '\n' terminates the
    // last line and is not itself a separator that begins a new (empty) line.
    let end = data.len();
    let mut newlines = 0usize;
    let mut i = end;
    // Skip a single trailing newline so it doesn't count as an extra line.
    let scan_end = if i > 0 && data[i - 1] == b'\n' {
        i - 1
    } else {
        i
    };
    i = scan_end;
    while i > 0 {
        if data[i - 1] == b'\n' {
            newlines += 1;
            if newlines == n {
                return &data[i..];
            }
        }
        i -= 1;
    }
    data
}

/// Hard cap on follow polling so the command never hangs forever even if a run
/// never writes a final `exited` status (supervisor vanished). 200ms/poll.
const FOLLOW_MAX_POLLS: u32 = 3000; // ~10 minutes ceiling
const FOLLOW_POLL_MS: u64 = 200;

/// Stream appends to `paths`, stopping when the run has exited and the streams
/// are drained, or when the poll cap is reached. Bounded — no infinite spin.
fn follow_bounded(id: &str, home: &Path, paths: &[std::path::PathBuf]) -> i32 {
    let mut offsets: Vec<u64> = paths
        .iter()
        .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .collect();

    let mut polls = 0u32;
    loop {
        let mut had_new = false;
        for (p, off) in paths.iter().zip(offsets.iter_mut()) {
            match append_from(p, off) {
                Ok(new) => had_new |= new,
                Err(e) => return die_lightr(&e),
            }
        }

        // Exit gate: the run has stopped (or is unresolvable) AND nothing new
        // this round ⇒ done. Treating Unknown/Err as terminal keeps a vanished
        // supervisor from pinning the loop to the poll cap.
        let terminal = matches!(
            run_status(home, id),
            Ok(RunStatus::Exited(_)) | Ok(RunStatus::Unknown) | Err(_)
        );
        if terminal && !had_new {
            return 0;
        }

        polls += 1;
        if polls >= FOLLOW_MAX_POLLS {
            // Bounded stop — honest, not a silent hang.
            eprintln!("lightr: logs --follow stopped at poll cap ({FOLLOW_MAX_POLLS})");
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(FOLLOW_POLL_MS));
    }
}

/// Write any bytes in `path` past `*offset` to stdout; advance `*offset`.
fn append_from(path: &Path, offset: &mut u64) -> lightr_core::Result<bool> {
    let (bytes, new_off) = bytes_after(path, *offset)?;
    if bytes.is_empty() {
        return Ok(false);
    }
    let mut out = std::io::stdout();
    out.write_all(&bytes)
        .map_err(lightr_core::LightrError::Io)?;
    out.flush().map_err(lightr_core::LightrError::Io)?;
    *offset = new_off;
    Ok(true)
}

/// Pure read-after-offset: bytes of `path` past `offset` and the new offset
/// (the file's full length). Missing file ⇒ empty + same offset. Lets follow be
/// tested deterministically without capturing stdout.
fn bytes_after(path: &Path, offset: u64) -> lightr_core::Result<(Vec<u8>, u64)> {
    if !path.exists() {
        return Ok((Vec::new(), offset));
    }
    let data = std::fs::read(path).map_err(lightr_core::LightrError::Io)?;
    let start = (offset as usize).min(data.len());
    let new_off = data.len() as u64;
    Ok((data[start..].to_vec(), new_off))
}

/// The single honest timestamp signal: the log file's mtime. Printed to stderr
/// so it never corrupts the log stream on stdout.
fn emit_timestamp_note(run_dir: &Path, stream: &LogStream, since: Option<&str>) {
    let mtime = stream_paths(run_dir, stream)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .filter_map(|m| m.modified().ok())
        .max();
    let when = mtime
        .map(format_systemtime)
        .unwrap_or_else(|| "unknown".to_string());
    if since.is_some() {
        eprintln!(
            "lightr: logs has no per-line timestamps; --since compares against \
             the log file's last-modified time ({when})"
        );
    } else {
        eprintln!(
            "lightr: logs has no per-line timestamps; -t reports the log file's \
             last-modified time ({when})"
        );
    }
}

/// `--since` honest semantics: with no per-line clock, the only honest cutoff is
/// the whole file's mtime. If the file was last written BEFORE the cutoff, there
/// is nothing "since" then ⇒ exclude all. Unparseable cutoff ⇒ include (lenient,
/// matching docker's best-effort disposition rather than failing closed here).
fn since_excludes_all(run_dir: &Path, stream: &LogStream, since: Option<&str>) -> bool {
    let Some(s) = since else { return false };
    let Some(cutoff) = parse_since(s) else {
        return false;
    };
    let mtime = stream_paths(run_dir, stream)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .filter_map(|m| m.modified().ok())
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .max();
    match mtime {
        Some(m) => m < cutoff,
        None => false,
    }
}

/// Parse a `--since` value: unix seconds. We avoid a chrono dep — only the
/// unix-seconds form is parsed precisely; any other string yields `None`
/// (lenient include, honest about the limitation in the stderr note above).
fn parse_since(s: &str) -> Option<u64> {
    s.trim().parse::<u64>().ok()
}

/// Format a SystemTime as unix seconds (honest, dependency-free).
fn format_systemtime(t: std::time::SystemTime) -> String {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format!("{}", d.as_secs()),
        Err(_) => "pre-epoch".to_string(),
    }
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
