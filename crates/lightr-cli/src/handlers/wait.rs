//! `lightr wait` handler — block until containers stop, print exit codes
//! (docker wait). For each target: resolve → id → block on `wait_run` → print
//! the exit code on its own stdout line, in argument order. Faithful to
//! `docker wait`: one code per line, all targets processed in order, exit 1 if
//! any target failed to resolve/wait, exit 2 if no targets were given.

use crate::lightr_home;

pub fn run(targets: &[String]) -> i32 {
    if targets.is_empty() {
        return 2;
    }

    let home = lightr_home();
    let mut any_failed = false;

    for t in targets {
        let id = match lightr_run::resolve(&home, t) {
            Ok(id) => id,
            Err(_) => {
                eprintln!("Error: No such container: {t}");
                any_failed = true;
                continue;
            }
        };

        match lightr_run::wait_run(&home, &id) {
            Ok(code) => println!("{code}"),
            Err(_) => {
                eprintln!("Error: No such container: {t}");
                any_failed = true;
            }
        }
    }

    if any_failed {
        1
    } else {
        0
    }
}
