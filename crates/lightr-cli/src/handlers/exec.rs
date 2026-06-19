//! `lightr exec` handler — exec a command in a run's context.

use lightr_run::exec_in;

use crate::{exit::die_lightr, lightr_home};

/// Read the `engine` field from `spec.json` in the given run directory.
///
/// Returns `Some(engine_string)` on success, `None` if the file is absent or
/// unreadable (the caller treats that as an unknown / non-vz run and lets
/// `exec_in` surface the real error).
fn read_engine(run_dir: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(run_dir.join("spec.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    // `engine` is serde-defaulted to "native" in SpecOnDisk; replicate that
    // default here so missing-field → "native" (same semantics, never panics).
    let engine = v
        .get("engine")
        .and_then(|e| e.as_str())
        .unwrap_or("native")
        .to_string();
    Some(engine)
}

pub fn run(id: &str, command: &[String]) -> i32 {
    let home = lightr_home();
    let run_dir = home.join("run").join(id);

    if !run_dir.exists() {
        eprintln!("lightr: unknown run id");
        return 2;
    }

    // Guard: `exec` cannot enter a Linux microVM guest from the host.
    // A vz run is a fully isolated VM — there are no namespaces to enter.
    if read_engine(&run_dir).as_deref() == Some("vz") {
        eprintln!(
            "lightr: exec is not supported for vz (microVM) runs; \
             run the command with `lightr run --engine vz ...` instead"
        );
        return 1;
    }

    match exec_in(&run_dir, command) {
        Ok(exit_code) => exit_code,
        Err(e) => die_lightr(&e),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::run;

    /// A vz run dir (spec.json with engine="vz") must exit 1 with a clear
    /// message BEFORE any exec attempt.
    #[test]
    fn exec_vz_run_exits_1() {
        // LIGHTR_HOME is process-global state: hold the crate-wide env lock for
        // the duration of this test to prevent races with other tests in this
        // binary that also call std::env::set_var("LIGHTR_HOME").
        let _env_guard = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::TempDir::new().expect("tmp dir");
        let run_dir = tmp.path().join("run").join("test-vz-id");
        std::fs::create_dir_all(&run_dir).expect("mkdir run_dir");

        // Write a minimal spec.json that looks like a vz run.
        let spec = serde_json::json!({
            "cwd": "/tmp",
            "command": ["sh"],
            "env_keys": [],
            "mounts": [],
            "detached": true,
            "created_at_unix": 0,
            "engine": "vz"
        });
        std::fs::write(run_dir.join("spec.json"), spec.to_string()).expect("write spec.json");

        // Point LIGHTR_HOME at tmp so `lightr_home()` resolves our run dir.
        std::env::set_var("LIGHTR_HOME", tmp.path().to_str().unwrap());

        let code = run("test-vz-id", &["true".to_string()]);
        assert_eq!(code, 1, "exec on a vz run must exit 1");
    }
}
