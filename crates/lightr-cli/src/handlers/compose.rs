//! `lightr compose up/down` handlers — build-spec-r3 §5.
//!
//! `up [-f F] [-p NAME] [--project-directory D] [--env-file E] [--profile P]…
//!  [--eager] [--ttl N]` and `down [-f F] [-p NAME]`. Exit 0 = success, 1 =
//! runtime error. Human `up`: `up: <n> services (listeners bound)` then one
//! `  <name>  (<eager|lazy>)` line per active service; `--json`:
//! `{"services":[...],"stack_dir":"<path>"}`.
//!
//! `down` reads the most-recent stack under `$LIGHTR_HOME/compose/`; when a
//! project is resolved (flag/env/`name:`/basename) the teardown is scoped to it.
//!
//! CMP-CLI-INTEGRATION: `up` runs the unified build path
//! `parse_compose_merged` (override deep-merge → `${VAR}` interpolation → parse
//! → lowering), with the dotenv/`--env-file` scope built at this call site.

use lightr_build::{
    compose_down, compose_up, dir_basename, parse_compose_merged, parse_compose_project_name,
    resolve_project_name, scope_from_project_dir, StackSpec, VarScope, OVERRIDE_FILENAMES,
};
use lightr_store::Store;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::exit::die_lightr;

/// CMP-CLI-INTEGRATION: the project directory for a compose run. Docker
/// `--project-directory` wins; else the compose file's parent (current dir for a
/// bare filename). Pure (args + current-dir fallback). Base for the default `.env`.
fn project_directory(compose_file: &str, project_directory: Option<&str>) -> PathBuf {
    if let Some(d) = project_directory {
        return PathBuf::from(d);
    }
    Path::new(compose_file)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// CMP-CLI-INTEGRATION: build the interpolation `VarScope` for `compose up`.
///
/// Compose precedence: the live process env WINS over the dotenv (lower) source.
/// No `--env-file` ⇒ `<project_dir>/.env` via the build crate's
/// [`scope_from_project_dir`]. `--env-file <path>` REPLACES the default `.env`;
/// the file is read via the compose `.env` subset adapter ([`parse_dotenv_subset`])
/// because the build crate's `parse_dotenv` is not re-exported and lib.rs is out
/// of WP scope (flagged). Fail-closed: an unreadable `--env-file` is an error.
fn build_scope(project_dir: &Path, env_file: Option<&str>) -> Result<VarScope, String> {
    let Some(env_file) = env_file else {
        // No --env-file: delegate to the build crate (reads <dir>/.env, process
        // env wins) — behavior-preserving, no reimplementation.
        return Ok(scope_from_project_dir(project_dir));
    };
    let text = std::fs::read_to_string(env_file)
        .map_err(|e| format!("compose up: cannot read --env-file {env_file}: {e}"))?;
    let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    // Lower precedence: the explicit env-file.
    for (k, v) in parse_dotenv_subset(&text) {
        env.insert(k, v);
    }
    // Higher precedence: the live process environment overwrites env-file values.
    for (k, v) in std::env::vars() {
        env.insert(k, v);
    }
    Ok(VarScope {
        args: std::collections::BTreeMap::new(),
        env,
    })
}

/// Compose `.env` subset for `--env-file` (CLI adapter faithful to the build
/// crate's `parse_dotenv`): `KEY=VAL`, `#`/blank skipped, `export ` stripped,
/// first `=` splits, one quote layer removed.
fn parse_dotenv_subset(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let bytes = value.as_bytes();
        let value = if bytes.len() >= 2
            && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
                || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
        {
            value[1..value.len() - 1].to_string()
        } else {
            value.to_string()
        };
        pairs.push((key.to_string(), value));
    }
    pairs
}

/// CMP-CLI-INTEGRATION: discover the docker-compose override file beside the
/// base — first existing of [`OVERRIDE_FILENAMES`] (precedence order), for
/// deep-merge via `parse_compose_merged`. None present ⇒ behavior-preserving.
fn discover_override(compose_file: &str) -> Option<String> {
    let base = Path::new(compose_file);
    let dir = base.parent().filter(|p| !p.as_os_str().is_empty());
    for name in OVERRIDE_FILENAMES {
        let candidate = match dir {
            Some(d) => d.join(name),
            None => PathBuf::from(name),
        };
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            return Some(text);
        }
    }
    None
}

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
    // Basename rung: compose file's parent (current dir for a bare filename).
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

#[allow(clippy::too_many_arguments)]
pub fn up(
    compose_file: &str,
    project: Option<&str>,
    project_dir: Option<&str>,
    env_file: Option<&str>,
    eager_all: bool,
    cli_profiles: &[String],
    ttl: u64,
    json: bool,
) -> i32 {
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

    // CMP-CLI-INTEGRATION: build the interpolation scope (.env / --env-file,
    // process-env-over-dotenv) + discover any override beside the base, then run
    // the unified build path (merge → interpolate → parse → lower). No override
    // and no ${VAR} ⇒ byte-identical to the old bare parse (behavior-preserving).
    let pdir = project_directory(compose_file, project_dir);
    let scope = match build_scope(&pdir, env_file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lightr: {e}");
            return 1;
        }
    };
    let override_yaml = discover_override(compose_file);
    let mut compose = match parse_compose_merged(&text, override_yaml.as_deref(), &scope) {
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

    // CMP-P1-PROFILES: union of `--profile` and COMPOSE_PROFILES (env read here).
    let active_profiles = union_profiles(
        cli_profiles,
        std::env::var("COMPOSE_PROFILES").ok().as_deref(),
    );

    let handle = match compose_up(&compose, &store, ttl, &project_name, &active_profiles) {
        Ok(h) => h,
        Err(e) => return die_lightr(&e),
    };

    // CMP-P1-PROFILES: report only the ACTIVE started services (`handle.services`),
    // not every declared one. With no profiles this is every service.
    let active: std::collections::HashSet<&str> =
        handle.services.iter().map(String::as_str).collect();

    if json {
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

// ── Tests (split out for godfile headroom — house convention) ──────────────────
#[cfg(test)]
#[path = "compose_tests.rs"]
mod tests;
