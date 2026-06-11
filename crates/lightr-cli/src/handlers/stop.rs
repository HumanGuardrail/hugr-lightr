//! `lightr stop` handler — stop a running instance.

use lightr_run::stop;

use crate::{
    exit::{die_internal, die_lightr},
    lightr_home,
};

pub fn run(id: &str, grace: u64) -> i32 {
    let home = lightr_home();
    let run_dir = home.join("run").join(id);

    if !run_dir.exists() {
        eprintln!("lightr: unknown run id");
        return 2;
    }

    match stop(&run_dir, grace) {
        Ok(exit_code) => exit_code,
        Err(e) => die_lightr(&e),
    }
}
