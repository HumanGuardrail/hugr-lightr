//! `lightr compose up/down` handlers — build-spec-r3 §5.
//!
//! Sub-verbs:
//!   compose up [-f compose.yml] [-p <project>] [--eager] [--ttl <secs=3600>]
//!   compose down [-f compose.yml] [-p <project>]
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
//! `down` reads the most-recent compose stack under $LIGHTR_HOME/compose/.
//! CMP-P1-PROJECT: when a project is resolved (flag/env/`name:`/basename), the
//! teardown is scoped to stacks recorded under that project name.

use lightr_build::{
    compose_down, compose_up, dir_basename, parse_compose, parse_compose_project_name,
    resolve_project_name, StackSpec,
};
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

/// CMP-P1-PROJECT: resolve the effective project name for a compose file.
///
/// Precedence (Docker): `-p`/`--project-name` flag > `COMPOSE_PROJECT_NAME`
/// env > the compose file's top-level `name:` field > the sanitized basename of
/// the compose file's directory. Reading the env here (not in the resolver)
/// keeps the resolver pure + tests parallel-safe. Fail-closed: an explicit
/// (flag/env/`name:`) value that sanitizes to nothing is an honest error.
fn resolve_project(
    project: Option<&str>,
    compose_file: &str,
    text: &str,
) -> lightr_core::Result<String> {
    let env = std::env::var("COMPOSE_PROJECT_NAME").ok();
    let env_ref = env.as_deref().filter(|s| !s.is_empty());
    let file_name = parse_compose_project_name(text)?;
    let path = std::path::Path::new(compose_file);
    // The project directory is the compose file's parent; an absolute file with
    // no parent (or a bare filename) falls back to the current dir's basename.
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let basename = dir_basename(&dir);
    resolve_project_name(project, env_ref, file_name.as_deref(), &basename)
}

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

/// CMP-P1-PROFILES: the union of `--profile` flags and `COMPOSE_PROFILES`.
///
/// `cli` is the repeatable `--profile` list; `env` is the raw `COMPOSE_PROFILES`
/// value (comma-separated, Docker grammar). Pure (env injected) so the union
/// rule is tested without touching process-global state. Order is CLI-first then
/// env, de-duplicated; blank/whitespace entries are dropped. An empty result
/// selects every service downstream (behavior-preserving).
fn union_profiles(cli: &[String], env: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let p = raw.trim();
        if !p.is_empty() && !out.iter().any(|e| e == p) {
            out.push(p.to_string());
        }
    };
    for p in cli {
        push(p);
    }
    if let Some(env) = env {
        for p in env.split(',') {
            push(p);
        }
    }
    out
}

pub fn up(
    compose_file: &str,
    project: Option<&str>,
    eager_all: bool,
    cli_profiles: &[String],
    ttl: u64,
    json: bool,
) -> i32 {
    // Read and parse the compose file
    let text = match std::fs::read_to_string(compose_file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lightr: compose up: cannot read {compose_file}: {e}");
            return 1;
        }
    };

    // CMP-P1-PROJECT: resolve the project name (cli>env>name>basename) BEFORE
    // any provisioning — a rejected explicit name fails closed here.
    let project_name = match resolve_project(project, compose_file, &text) {
        Ok(p) => p,
        Err(e) => return die_lightr(&e),
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

    // CMP-P1-PROFILES: union of `--profile` and COMPOSE_PROFILES, read here at
    // the call site (resolver stays pure + parallel-safe).
    let active_profiles = union_profiles(
        cli_profiles,
        std::env::var("COMPOSE_PROFILES").ok().as_deref(),
    );

    let handle = match compose_up(&compose, &store, ttl, &project_name, &active_profiles) {
        Ok(h) => h,
        Err(e) => return die_lightr(&e),
    };

    // CMP-P1-PROFILES: report only the ACTIVE services that were actually
    // started (`handle.services`), not every declared service — profile-gated
    // services excluded from the start are not listed. With no profiles this is
    // every service (behavior-preserving).
    let active: std::collections::HashSet<&str> =
        handle.services.iter().map(String::as_str).collect();

    if json {
        // Build JSON output from the active compose services
        let svc_json: Vec<ComposeServiceJson> = compose
            .services
            .iter()
            .filter(|s| active.contains(s.name.as_str()))
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
        let active_svcs: Vec<&_> = compose
            .services
            .iter()
            .filter(|s| active.contains(s.name.as_str()))
            .collect();
        let n = active_svcs.len();
        println!("up: {n} services (listeners bound)");
        for svc in active_svcs {
            let kind = if svc.eager { "eager" } else { "lazy" };
            println!("  {}  ({kind})", svc.name);
        }
    }

    0
}

// ── `compose down` handler ────────────────────────────────────────────────────

/// The project name recorded in a stack dir's `spec.json`, if readable.
/// Pre-CMP-P1-PROJECT specs (no `project` field) read back as `"default"` via
/// the model's serde default.
fn stack_project(stack_dir: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(stack_dir.join("spec.json")).ok()?;
    let spec: StackSpec = serde_json::from_slice(&bytes).ok()?;
    Some(spec.project)
}

/// Resolve the stack directory for `compose down`.
///
/// Strategy: walk `$LIGHTR_HOME/compose/` and return the most-recently
/// created subdirectory (name is `<nanos>-<pid>` so lexicographic sort
/// gives newest-last). If none found, return `None`.
///
/// CMP-P1-PROJECT: when `project` is `Some`, only stacks whose recorded
/// `project` matches are considered, so `compose down -p A` never tears down
/// project B (the projects-don't-collide invariant). When `None`, behavior is
/// preserved exactly: the newest stack regardless of project.
fn resolve_latest_stack(project: Option<&str>) -> Option<std::path::PathBuf> {
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
        .filter(|p| match project {
            Some(name) => stack_project(p).as_deref() == Some(name),
            None => true,
        })
        .collect();
    // Sort ascending by name (nanos prefix) ⇒ last = newest
    entries.sort();
    entries.into_iter().last()
}

/// `down` resolves the project name (cli>env>`name:`>basename) only when it
/// has a compose file to read for the `name:`/basename rungs; otherwise it
/// honors just the flag/env and falls through to the un-filtered newest stack
/// (today's behavior) when neither is given.
fn down_project(
    project: Option<&str>,
    compose_file: Option<&str>,
) -> lightr_core::Result<Option<String>> {
    // An explicit flag always selects a project.
    if let Some(p) = project {
        return resolve_project(Some(p), compose_file.unwrap_or("compose.yml"), "").map(Some);
    }
    // COMPOSE_PROJECT_NAME alone also scopes the teardown.
    if let Ok(env) = std::env::var("COMPOSE_PROJECT_NAME") {
        if !env.is_empty() {
            return resolve_project(None, compose_file.unwrap_or("compose.yml"), "").map(Some);
        }
    }
    // A compose file lets us derive the project from `name:`/basename.
    if let Some(cf) = compose_file {
        if let Ok(text) = std::fs::read_to_string(cf) {
            return resolve_project(None, cf, &text).map(Some);
        }
    }
    // No flag, env, or readable file ⇒ preserve today's "newest stack" behavior.
    Ok(None)
}

pub fn down(compose_file: Option<&str>, project: Option<&str>) -> i32 {
    let scope = match down_project(project, compose_file) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let stack_dir = match resolve_latest_stack(scope.as_deref()) {
        Some(d) => d,
        None => {
            match &scope {
                Some(p) => eprintln!("lightr: compose down: no active stack for project '{p}'"),
                None => eprintln!("lightr: compose down: no active compose stack found"),
            }
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
        let code = super::up("/no/such/file.yml", None, false, &[], 3600, false);
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
        let code = super::up(f.to_str().unwrap(), None, false, &[], 3600, false);
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
        let code = super::down(None, None);
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
        let result = super::resolve_latest_stack(None);
        std::env::remove_var("LIGHTR_HOME");
        assert!(result.is_none());
    }

    // ── CMP-P1-PROFILES: union_profiles (pure, env injected; parallel-safe) ──

    #[test]
    fn union_profiles_empty_when_nothing_given() {
        // Behavior-preserving: no --profile, no COMPOSE_PROFILES ⇒ empty union
        // ⇒ all services active downstream.
        assert!(super::union_profiles(&[], None).is_empty());
        assert!(super::union_profiles(&[], Some("")).is_empty());
    }

    #[test]
    fn union_profiles_cli_only() {
        let cli = vec!["dev".to_string(), "debug".to_string()];
        assert_eq!(super::union_profiles(&cli, None), cli);
    }

    #[test]
    fn union_profiles_env_only_comma_separated() {
        assert_eq!(
            super::union_profiles(&[], Some("dev,prod")),
            vec!["dev".to_string(), "prod".to_string()]
        );
    }

    #[test]
    fn union_profiles_union_cli_and_env_dedup() {
        // CLI-first, env appended, duplicates dropped, blanks trimmed.
        let cli = vec!["dev".to_string()];
        assert_eq!(
            super::union_profiles(&cli, Some(" dev , prod , ")),
            vec!["dev".to_string(), "prod".to_string()]
        );
    }
}
