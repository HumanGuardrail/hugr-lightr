//! `lightr logs` handler — stream logs from a run.

use lightr_run::{logs, LogStream};

use crate::{exit::die_lightr, lightr_home};

pub fn run(id: &str, stderr: bool, both: bool, follow: bool) -> i32 {
    let home = lightr_home();
    let run_dir = home.join("run").join(id);

    if !run_dir.exists() {
        eprintln!("lightr: unknown run id");
        return 2;
    }

    let stream = if both {
        LogStream::Both
    } else if stderr {
        LogStream::Stderr
    } else {
        LogStream::Stdout
    };

    match logs(&run_dir, stream, follow) {
        Ok(()) => 0,
        Err(e) => die_lightr(&e),
    }
}
