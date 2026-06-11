//! `lightr exec` handler — exec a command in a run's context.

use lightr_run::exec_in;

use crate::{exit::die_lightr, lightr_home};

pub fn run(id: &str, command: &[String]) -> i32 {
    let home = lightr_home();
    let run_dir = home.join("run").join(id);

    if !run_dir.exists() {
        eprintln!("lightr: unknown run id");
        return 2;
    }

    match exec_in(&run_dir, command) {
        Ok(exit_code) => exit_code,
        Err(e) => die_lightr(&e),
    }
}
