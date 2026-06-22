//! `lightr container` handlers — docker `container <subcmd>` parity.
//!
//! Currently the maintenance verb `container prune`: remove all STOPPED containers
//! (the daemonless analog of `docker container prune`). The candidate set is every
//! run whose `status` file proves it `exited` (`lifecycle::list_stopped_runs`); a
//! RUNNING run is never touched. Without `-f` it is a DRY RUN — it prints what it
//! WOULD remove and removes nothing (docker prints an interactive confirmation; we
//! print the preview, since there is no TTY-prompt path in the daemonless CLI).
//! With `-f` it removes each candidate via the shared `lifecycle::remove_run`
//! primitive (which also frees the run's registry name) and prints Docker's
//! `Deleted Containers:` / `Total reclaimed space:` report.

use lightr_run::{list_stopped_runs, remove_run};

use crate::cli::cmd::ContainerCmd;
use crate::exit::die_lightr;
use crate::lightr_home;

pub fn run(subcmd: ContainerCmd) -> i32 {
    match subcmd {
        ContainerCmd::Prune { force } => prune(force),
    }
}

fn prune(force: bool) -> i32 {
    let home = lightr_home();

    let stopped = match list_stopped_runs(&home) {
        Ok(ids) => ids,
        Err(e) => return die_lightr(&e),
    };

    // Dry run (no -f): preview only, remove nothing (fail-closed against an
    // accidental mass-delete). Docker prompts interactively; the daemonless CLI
    // has no prompt path, so it prints the would-remove preview instead.
    if !force {
        if stopped.is_empty() {
            println!("Total reclaimed space: 0B");
        } else {
            println!(
                "WARNING: This will remove all stopped containers. Re-run with -f to confirm."
            );
            println!("Would remove {} container(s):", stopped.len());
            for id in &stopped {
                println!("{id}");
            }
        }
        return 0;
    }

    // -f: actually remove each stopped run (reuses the lifecycle primitive, which
    // releases the registry name too). Continue-on-error: a failure on one is
    // reported but never halts the rest (docker prune is best-effort per item).
    let mut deleted: Vec<String> = Vec::new();
    let mut any_failed = false;
    for id in stopped {
        // `force = false`: these are already proven-stopped, so we never kill a
        // live child here — a run that raced into RUNNING is honestly refused.
        match remove_run(&home, &id, false) {
            Ok(()) => deleted.push(id),
            Err(e) => {
                eprintln!("lightr: prune {id}: {e}");
                any_failed = true;
            }
        }
    }

    if deleted.is_empty() {
        println!("Total reclaimed space: 0B");
    } else {
        println!("Deleted Containers:");
        for id in &deleted {
            println!("{id}");
        }
        println!();
        // Run dirs are tiny metadata; lightr does not track per-run reclaimed
        // bytes (the heavy data lives in the shared CAS, reclaimed by `gc`), so we
        // report 0B honestly rather than fabricate a figure (tense-law).
        println!("Total reclaimed space: 0B");
    }

    i32::from(any_failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Materialize a run dir `<home>/run/<id>` with a terminal `exited` status.
    fn make_exited(home: &std::path::Path, id: &str) {
        let dir = home.join("run").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("status"), "exited 0").unwrap();
    }

    /// Materialize a RUNNING run dir: a live ctl endpoint + OUR pid (alive for the
    /// test) so `is_running` reports true via the real detection path. The ctl
    /// endpoint sentinel is `ctl.sock` on unix (the only host these tests run on —
    /// the windows-cross gate is clippy-compile-only, never `cargo test`).
    #[cfg(unix)]
    fn make_running(home: &std::path::Path, id: &str) {
        let dir = home.join("run").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ctl.sock"), b"live").unwrap();
        std::fs::write(dir.join("pid"), format!("{}", std::process::id())).unwrap();
    }

    #[test]
    fn dry_run_removes_nothing() {
        let _g = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::TempDir::new().expect("tmp");
        std::env::set_var("LIGHTR_HOME", tmp.path());
        make_exited(tmp.path(), "exited-a");
        make_exited(tmp.path(), "exited-b");

        let code = prune(false);

        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 0);
        // Dry run: both dirs must still exist.
        assert!(tmp.path().join("run").join("exited-a").exists());
        assert!(tmp.path().join("run").join("exited-b").exists());
    }

    #[cfg(unix)]
    #[test]
    fn force_removes_only_exited() {
        let _g = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::TempDir::new().expect("tmp");
        std::env::set_var("LIGHTR_HOME", tmp.path());
        make_exited(tmp.path(), "exited-a");
        make_exited(tmp.path(), "exited-b");
        make_running(tmp.path(), "running-c");

        let code = prune(true);

        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 0);
        // The 2 exited dirs are gone; the running one is untouched.
        assert!(!tmp.path().join("run").join("exited-a").exists());
        assert!(!tmp.path().join("run").join("exited-b").exists());
        assert!(tmp.path().join("run").join("running-c").exists());
    }
}
