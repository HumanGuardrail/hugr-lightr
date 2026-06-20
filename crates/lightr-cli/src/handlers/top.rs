//! `lightr top` handler — list the processes running in a container
//! (docker top).
//!
//! macOS/native is PROCESS-based (no PID namespace), so `docker top`'s "the
//! processes inside the container" is honestly "the run's supervisor process
//! and its direct children" — sampled from `ps(1)` (+ `pgrep -P` for the tree).
//! We print a `ps -ef`-style table (the docker-top shape). A run that is not
//! running is "container <id> is not running" (exit 1, Docker parity); nothing
//! is fabricated (tense-law).

use lightr_run::resolve;

use crate::handlers::runproc::{pid_for_id, query_top};
use crate::lightr_home;

const HEADER: &str = "PID    USER             TIME      COMMAND";

pub fn run(target: &str) -> i32 {
    let home = lightr_home();

    // ref → id. Docker `top <missing>` → "No such container" (exit 1).
    let id = match resolve(&home, target) {
        Ok(id) => id,
        Err(_) => {
            eprintln!("lightr: No such container: {target}");
            return 1;
        }
    };

    // A run is "running" iff it has a live supervisor pid producing ps rows.
    let pid = match pid_for_id(&home, &id) {
        Some(p) => p,
        None => {
            eprintln!("lightr: container {id} is not running");
            return 1;
        }
    };

    let rows = query_top(pid);
    if rows.is_empty() {
        // pid file present but the process is gone — honestly not running.
        eprintln!("lightr: container {id} is not running");
        return 1;
    }

    println!("{HEADER}");
    for r in &rows {
        println!("{:<6} {:<16} {:<9} {}", r.pid, r.user, r.time, r.command);
    }
    0
}

#[cfg(test)]
#[path = "top_tests.rs"]
mod tests;
