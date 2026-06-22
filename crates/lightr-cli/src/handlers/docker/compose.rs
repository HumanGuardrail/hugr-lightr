//! `docker compose up/down` → `lightr compose` translation, split from the
//! docker shim's `mod.rs` for godfile headroom.
//!
//! FIX #74: the flag loop no longer SILENTLY ignores unrecognized flags (the
//! old `_ => {}` arm). `-d/--detach` is accepted (lightr compose is supervised /
//! daemonless — detached IS its model). The docker flags native genuinely does
//! not support yet (`--build`/`--remove-orphans`/`--volumes`) are honest errors,
//! never a silent drop; an unrecognized flag is likewise an honest error.

use super::flag_err::unsupported_flag;
use super::note_translation;

/// Honest error for a `docker compose` flag native does not support yet (exit 2).
fn unsupported_compose(flag: &str) -> i32 {
    eprintln!(
        "lightr docker: compose: {flag} is not yet supported by the shim (lightr compose has no \
         equivalent) — not silently dropped"
    );
    2
}

/// Translate `docker compose <up|down> [...]` to the corresponding lightr
/// compose handler. Flag parsing is the minimal docker-compatible subset.
pub(super) fn translate_compose(args: &[String], json: bool) -> i32 {
    if args.is_empty() {
        eprintln!("lightr docker: compose: missing subcommand");
        return 2;
    }
    match args[0].as_str() {
        "up" => translate_up(&args[1..], json),
        "down" => translate_down(&args[1..]),
        sub => {
            eprintln!(
                "lightr docker: compose: unsupported subcommand '{sub}' — supported: up|down"
            );
            2
        }
    }
}

/// `docker compose up [-f F] [-p NAME] [--profile P]… [--eager] [--ttl N]`.
fn translate_up(rest: &[String], json: bool) -> i32 {
    let mut compose_file = "compose.yml".to_string();
    let mut project: Option<String> = None;
    let mut eager = false;
    // CMP-P1-PROFILES: `docker compose up --profile <name>` (repeatable).
    let mut profiles: Vec<String> = Vec::new();
    let mut ttl: u64 = 3600;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-f" | "--file" => {
                i += 1;
                if i < rest.len() {
                    compose_file = rest[i].clone();
                }
            }
            "-p" | "--project-name" => {
                i += 1;
                if i < rest.len() {
                    project = Some(rest[i].clone());
                }
            }
            "--profile" => {
                i += 1;
                if i < rest.len() {
                    profiles.push(rest[i].clone());
                }
            }
            "--eager" => eager = true,
            "--ttl" => {
                i += 1;
                if i < rest.len() {
                    ttl = rest[i].parse().unwrap_or(3600);
                }
            }
            // `-d/--detach`: lightr compose runs under a supervisor (daemonless,
            // detached by design), so docker's `-d` maps to the natural mode —
            // accepted as a no-op rather than errored.
            "-d" | "--detach" => {}
            // Docker flags native compose does not implement yet — honest error.
            "--build" | "--remove-orphans" | "--volumes" => {
                return unsupported_compose(rest[i].as_str())
            }
            other if other.starts_with('-') => return unsupported_flag("compose up", other),
            // A bare positional in `compose up` is a SERVICE name filter, which
            // lightr compose does not implement — honest error, never a drop.
            other => {
                eprintln!(
                    "lightr docker: compose up: service filter '{other}' is not yet supported by \
                     the shim (lightr compose starts the whole stack)"
                );
                return 2;
            }
        }
        i += 1;
    }
    note_translation("compose", &["up", "-f", &compose_file]);
    crate::handlers::compose::up(
        &compose_file,
        project.as_deref(),
        eager,
        &profiles,
        ttl,
        json,
    )
}

/// `docker compose down [-f F] [-p NAME]`.
fn translate_down(rest: &[String]) -> i32 {
    let mut compose_file: Option<String> = None;
    let mut project: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-f" | "--file" => {
                i += 1;
                if i < rest.len() {
                    compose_file = Some(rest[i].clone());
                }
            }
            "-p" | "--project-name" => {
                i += 1;
                if i < rest.len() {
                    project = Some(rest[i].clone());
                }
            }
            // Docker flags native `compose down` does not implement yet.
            "--remove-orphans" | "--volumes" | "-v" => {
                return unsupported_compose(rest[i].as_str())
            }
            other if other.starts_with('-') => return unsupported_flag("compose down", other),
            other => {
                eprintln!(
                    "lightr docker: compose down: unexpected argument '{other}' (lightr compose \
                     down takes no positionals)"
                );
                return 2;
            }
        }
        i += 1;
    }
    note_translation("compose", &["down"]);
    crate::handlers::compose::down(compose_file.as_deref(), project.as_deref())
}
