//! `lightr attach` handler — attach to a RUNNING container's output (docker attach).
//!
//! HONEST scope on the daemonless runtime: after `-d` detach, the SUPERVISOR (not
//! the CLI) owns the child's stdin/stdout/stderr file descriptors. There is NO live
//! stdin FD the CLI can reattach to, so a faithful interactive `docker attach`
//! (forwarding the terminal into the container) is structurally impossible here.
//! Rather than fake it, `attach` implements the OUTPUT-FOLLOW half: it tails the
//! run's `stdout.log` + `stderr.log` until the container exits or Ctrl-C —
//! equivalent to `docker attach --no-stdin` (a.k.a. `logs -f`). The `--help` text
//! and a one-line stderr note state the stdin limitation plainly (no silent fake).
//!
//! Docker parity on the error path: attaching to a STOPPED container prints
//! `You cannot attach to a stopped container` and exits 1.

use std::io::Write;
use std::path::Path;

use lightr_run::{resolve, run_status, RunStatus};

use crate::exit::die_lightr;
use crate::lightr_home;

/// Honest one-line disclosure printed to stderr before streaming: stdin attach is
/// not supported on the daemonless runtime (the supervisor owns the child FDs).
const NO_STDIN_NOTE: &str =
    "lightr: attach follows container output only; stdin is not supported on the \
     daemonless runtime (the supervisor owns the child's FDs — like `docker attach --no-stdin`)";

pub fn run(id: &str) -> i32 {
    let home = lightr_home();

    // ref/name/id-prefix → run id (Docker parity: unresolvable ⇒ no-such-container).
    let resolved = match resolve(&home, id) {
        Ok(rid) => rid,
        Err(_) => {
            eprintln!("Error: No such container: {id}");
            return 1;
        }
    };

    // Docker: `attach` to a stopped container is refused with this exact message
    // and exit 1. Only a RUNNING container has live output to follow.
    match run_status(&home, &resolved) {
        Ok(RunStatus::Running) => {}
        Ok(_) => {
            eprintln!("Error: You cannot attach to a stopped container");
            return 1;
        }
        Err(e) => return die_lightr(&e),
    }

    // Honest disclosure (stderr so it never corrupts the followed stdout stream).
    eprintln!("{NO_STDIN_NOTE}");

    let run_dir = home.join("run").join(&resolved);
    let paths = vec![run_dir.join("stdout.log"), run_dir.join("stderr.log")];
    follow_until_exit(&resolved, &home, &paths)
}

/// Hard poll cap so attach never hangs forever (no-daemon discipline: nothing of
/// ours spins unbounded), mirroring the `logs --follow` ceiling.
const FOLLOW_MAX_POLLS: u32 = 3000; // ~10 minutes at 200ms/poll
const FOLLOW_POLL_MS: u64 = 200;

/// Stream appends to `paths` to stdout, stopping when the run has exited and the
/// streams are drained, or at the poll cap. Bounded — never an infinite spin.
fn follow_until_exit(id: &str, home: &Path, paths: &[std::path::PathBuf]) -> i32 {
    // Start from the CURRENT end of each stream — docker `attach` shows output
    // produced from the attach point onward, not the full backlog (`logs` is for
    // the backlog). New bytes after attach stream live.
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

        // Exit gate: the run has stopped (or is unresolvable) AND nothing new this
        // round ⇒ done. Treating Unknown/Err as terminal keeps a vanished
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
            eprintln!("lightr: attach stopped at poll cap ({FOLLOW_MAX_POLLS})");
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

/// Pure read-after-offset: bytes of `path` past `offset` + the new offset (the
/// file's full length). Missing file ⇒ empty + same offset (a stream may have
/// produced no output yet). Pure ⇒ unit-testable without capturing stdout.
fn bytes_after(path: &Path, offset: u64) -> lightr_core::Result<(Vec<u8>, u64)> {
    if !path.exists() {
        return Ok((Vec::new(), offset));
    }
    let data = std::fs::read(path).map_err(lightr_core::LightrError::Io)?;
    let start = (offset as usize).min(data.len());
    let new_off = data.len() as u64;
    Ok((data[start..].to_vec(), new_off))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_after_reads_only_the_tail() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let f = tmp.path().join("stdout.log");
        std::fs::write(&f, b"hello world").unwrap();
        // From offset 6 we get only the tail past it.
        let (bytes, off) = bytes_after(&f, 6).unwrap();
        assert_eq!(bytes, b"world");
        assert_eq!(off, 11);
    }

    #[test]
    fn bytes_after_missing_file_is_empty() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let f = tmp.path().join("nope.log");
        let (bytes, off) = bytes_after(&f, 0).unwrap();
        assert!(bytes.is_empty());
        assert_eq!(off, 0);
    }

    #[test]
    fn attach_to_missing_container_exits_1() {
        let _g = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::TempDir::new().expect("tmp");
        std::env::set_var("LIGHTR_HOME", tmp.path());
        let code = run("does-not-exist");
        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 1, "attach on a missing container must exit 1");
    }

    #[test]
    fn attach_to_stopped_container_exits_1() {
        let _g = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::TempDir::new().expect("tmp");
        std::env::set_var("LIGHTR_HOME", tmp.path());
        // Materialize a stopped run: a run dir with a terminal `exited` status and
        // no live ctl endpoint ⇒ run_status = Exited.
        let dir = tmp.path().join("run").join("stopped-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("status"), "exited 0").unwrap();
        let code = run("stopped-1");
        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(
            code, 1,
            "attach on a stopped container must exit 1 (docker parity)"
        );
    }
}
