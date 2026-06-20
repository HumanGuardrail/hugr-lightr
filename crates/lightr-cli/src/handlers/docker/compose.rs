//! `docker compose up/down` → `lightr compose` translation, split from the
//! docker shim's `mod.rs` for godfile headroom. Behavior is identical to the
//! inlined form; only the location changed.

use super::note_translation;

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
            _ => {}
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
            _ => {}
        }
        i += 1;
    }
    note_translation("compose", &["down"]);
    crate::handlers::compose::down(compose_file.as_deref(), project.as_deref())
}
