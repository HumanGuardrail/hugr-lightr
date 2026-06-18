//! `lightr compose up/down` handlers — build-spec-r3 §5.
//!
//! Sub-verbs:
//!   compose up [-f compose.yml] [--eager] [--ttl <secs=3600>]
//!   compose down [-f compose.yml]
//!
//! Exit codes:
//!   0  — success (listeners bound / stack torn down)
//!   1  — runtime error
//!
//! `up` human output:
//!   `up: <n> services (listeners bound)`
//!   followed by one line per service: `  <name>  (<eager|lazy>)`
//!
//! `up` --json: `{"services":[...],"stack_dir":"<path>"}`
//!
//! `down` reads the most-recent compose stack under $LIGHTR_HOME/compose/
//! unless a specific stack dir is resolved from the compose file's presence.

use lightr_build::{compose_down, compose_up, parse_compose};
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

// ── JSON output for `compose up` ──────────────────────────────────────────────

#[derive(Serialize)]
struct ComposeUpJson {
    services: Vec<ComposeServiceJson>,
    stack_dir: String,
}

#[derive(Serialize)]
struct ComposeServiceJson {
    name: String,
    eager: bool,
    image_ref: String,
    ports: Vec<(u16, u16)>,
}

// ── `compose up` handler ──────────────────────────────────────────────────────

pub fn up(compose_file: &str, eager_all: bool, ttl: u64, json: bool) -> i32 {
    // Read and parse the compose file
    let text = match std::fs::read_to_string(compose_file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lightr: compose up: cannot read {compose_file}: {e}");
            return 1;
        }
    };

    let mut compose = match parse_compose(&text) {
        Ok(c) => c,
        Err(e) => return die_lightr(&e),
    };

    // --eager flag marks all services eager
    if eager_all {
        for svc in &mut compose.services {
            svc.eager = true;
        }
    }

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let handle = match compose_up(&compose, &store, ttl) {
        Ok(h) => h,
        Err(e) => return die_lightr(&e),
    };

    if json {
        // Build JSON output from the compose services
        let svc_json: Vec<ComposeServiceJson> = compose
            .services
            .iter()
            .map(|s| ComposeServiceJson {
                name: s.name.clone(),
                eager: s.eager,
                image_ref: s.image_ref.clone(),
                ports: s.ports.clone(),
            })
            .collect();
        let out = ComposeUpJson {
            services: svc_json,
            stack_dir: handle.stack_dir.to_string_lossy().into_owned(),
        };
        println!(
            "{}",
            serde_json::to_string(&out).expect("serialize compose up")
        );
    } else {
        let n = compose.services.len();
        println!("up: {n} services (listeners bound)");
        for svc in &compose.services {
            let kind = if svc.eager { "eager" } else { "lazy" };
            println!("  {}  ({kind})", svc.name);
        }
    }

    0
}

// ── `compose down` handler ────────────────────────────────────────────────────

/// Resolve the stack directory for `compose down`.
///
/// Strategy: walk `$LIGHTR_HOME/compose/` and return the most-recently
/// created subdirectory (name is `<nanos>-<pid>` so lexicographic sort
/// gives newest-last). If none found, return an error.
fn resolve_latest_stack() -> Option<std::path::PathBuf> {
    let home = crate::lightr_home();
    let compose_dir = home.join("compose");
    if !compose_dir.is_dir() {
        return None;
    }
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&compose_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    // Sort ascending by name (nanos prefix) ⇒ last = newest
    entries.sort();
    entries.into_iter().last()
}

pub fn down(compose_file: Option<&str>) -> i32 {
    // We don't currently use compose_file to locate the stack (no reverse mapping)
    // — we pick the most-recent stack dir from $LIGHTR_HOME/compose/.
    let _ = compose_file;

    let stack_dir = match resolve_latest_stack() {
        Some(d) => d,
        None => {
            eprintln!("lightr: compose down: no active compose stack found");
            return 1;
        }
    };

    match compose_down(&stack_dir) {
        Ok(()) => 0,
        Err(e) => die_lightr(&e),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    /// `compose up` with a missing file ⇒ exit 1
    #[test]
    fn compose_up_missing_file_exits_1() {
        let code = super::up("/no/such/file.yml", false, 3600, false);
        assert_eq!(code, 1, "missing compose file must exit 1");
    }

    /// `compose up` with an empty services block ⇒ exit 0 (nothing to bind)
    #[test]
    fn compose_up_empty_services_exits_0() {
        let _env = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", tmp.path());
        let f = tmp.path().join("compose.yml");
        std::fs::write(&f, "services:\n").unwrap();
        let code = super::up(f.to_str().unwrap(), false, 3600, false);
        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 0);
    }

    /// `compose down` with no active stack ⇒ exit 1
    #[test]
    fn compose_down_no_stack_exits_1() {
        let _env = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", tmp.path());
        let code = super::down(None);
        std::env::remove_var("LIGHTR_HOME");
        assert_eq!(code, 1, "no active stack must exit 1");
    }

    /// resolve_latest_stack: returns None when compose dir is absent
    #[test]
    fn resolve_latest_stack_absent_dir_is_none() {
        let _env = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        std::env::set_var("LIGHTR_HOME", tmp.path());
        let result = super::resolve_latest_stack();
        std::env::remove_var("LIGHTR_HOME");
        assert!(result.is_none());
    }
}
