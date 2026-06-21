//! `lightr stop` handler — stop a running instance.

use lightr_run::stop;

use crate::{exit::die_lightr, lightr_home};

pub fn run(id: &str, grace: u64) -> i32 {
    let home = lightr_home();
    // WP-RUNFLAGS: resolve a `--name` (or id-prefix) to the run id, like `rm`, so
    // `stop <name>` works. Unresolvable ⇒ "No such container" + exit 1 (Docker
    // parity, WP-EXIT-CODE). A bare existing id still resolves to itself.
    let resolved = match lightr_run::resolve(&home, id) {
        Ok(rid) => rid,
        Err(_) => {
            eprintln!("Error: No such container: {id}");
            return 1;
        }
    };
    let run_dir = home.join("run").join(&resolved);

    // Docker `stop <missing>` → "No such container" + exit 1 (WP-EXIT-CODE).
    if !run_dir.exists() {
        eprintln!("Error: No such container: {id}");
        return 1;
    }

    match stop(&run_dir, grace) {
        Ok(exit_code) => exit_code,
        Err(e) => die_lightr(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::run;

    /// Docker parity (WP-EXIT-CODE): `stop <missing>` → exit 1, not 2.
    #[test]
    fn missing_container_exits_1() {
        // LIGHTR_HOME is process-global; serialize under the crate env lock so
        // this stays parallel-safe with the rest of the binary's tests.
        let _g = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::TempDir::new().expect("tmp dir");
        std::env::set_var("LIGHTR_HOME", tmp.path());
        let code = run("does-not-exist", 10);
        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 1, "stop on a missing container must exit 1");
    }
}
