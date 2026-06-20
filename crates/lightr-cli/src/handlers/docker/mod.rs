//! `lightr docker <args…>` handler — build-spec-r3 §4 (docker CLI compat).
//!
//! Translates a useful docker CLI subset to lightr verbs.
//! Always prints to stderr: `lightr docker: → lightr <verb> …` for transparency.
//!
//! Mapping table:
//!   docker build -t TAG [-f F] CTX      → build -t TAG [-f F] CTX
//!   docker run [IMG] [CMD…]             → import-if-needed + run
//!   docker pull IMG                     → oci pull IMG --name <sanitized>
//!   docker images                       → list all refs (store.list_refs), one per line
//!   docker ps                           → ps
//!   docker compose up/down [...]        → compose up/down [...]
//!
//! Unsupported subcommand ⇒ exit 2 with exact message:
//!   "lightr docker: unsupported '<x>' — supported: build|run|pull|images|ps|compose"
//!
//! Flag mapping is minimal: only flags documented above are translated.
//! docker run <ref> <cmd…>: if ref is a known store ref, hydrate it to a
//! temp dir and call run --rootfs <ref>; if not found, dispatch as cwd run.

use lightr_store::Store;

use crate::exit::die_lightr;

// ── ref name sanitizer for `docker pull` ──────────────────────────────────────

/// Sanitize a docker image reference into a valid lightr ref name.
///
/// Docker refs can contain '/', ':', and other chars not valid in lightr refs.
/// Strategy: replace '/' and ':' with '-', prefix with `@docker/` to namespace.
///
/// Examples:
///   "alpine"                    → "@docker/alpine"
///   "nginx:1.25"                → "@docker/nginx-1.25"
///   "ghcr.io/owner/repo:tag"    → "@docker/ghcr.io-owner-repo-tag"
pub fn sanitize_docker_ref(image: &str) -> String {
    // Replace '/' → '-', ':' → '-' in the image portion, then prefix.
    // Keep dots (valid in lightr refs).
    let sanitized = image.replace(['/', ':'], "-");
    format!("@docker/{sanitized}")
}

// ── Transparency helper ───────────────────────────────────────────────────────

fn note_translation(lightr_verb: &str, lightr_args: &[&str]) {
    let args_str = lightr_args.join(" ");
    if args_str.is_empty() {
        eprintln!("lightr docker: → lightr {lightr_verb}");
    } else {
        eprintln!("lightr docker: → lightr {lightr_verb} {args_str}");
    }
}

// ── docker build translation ──────────────────────────────────────────────────

/// Parse `docker build [-t TAG] [-f FILE] [--file FILE] <CTX>` and dispatch to
/// `handlers::build::run`.
fn translate_build(args: &[String], json: bool, explain: bool) -> i32 {
    let mut tag: Option<String> = None;
    let mut dockerfile: Option<String> = None;
    let mut context: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" | "--tag" => {
                i += 1;
                if i < args.len() {
                    tag = Some(args[i].clone());
                }
            }
            "-f" | "--file" => {
                i += 1;
                if i < args.len() {
                    dockerfile = Some(args[i].clone());
                }
            }
            arg if arg.starts_with("--tag=") => {
                tag = Some(arg["--tag=".len()..].to_string());
            }
            arg if arg.starts_with("--file=") => {
                dockerfile = Some(arg["--file=".len()..].to_string());
            }
            _ => {
                // Positional: context dir
                context = Some(args[i].clone());
            }
        }
        i += 1;
    }

    let ctx = match context {
        Some(c) => c,
        None => {
            eprintln!("lightr docker: build: missing context argument");
            return 2;
        }
    };

    let name = tag.unwrap_or_else(|| "latest".to_string());

    // Transparency note
    let df_display = dockerfile.as_deref().unwrap_or("<context>/Dockerfile");
    note_translation(
        "build",
        &["-f", df_display, "-t", &name, "--engine", "native", &ctx],
    );

    // The docker shim does not yet translate `--build-arg` (deferred); pass none.
    crate::handlers::build::run(
        &ctx,
        dockerfile.as_deref(),
        &name,
        "native",
        &[],
        json,
        explain,
    )
}

// ── docker run translation ────────────────────────────────────────────────────

/// `docker run <ref> <cmd…>` → if <ref> is a known store ref, add --rootfs
/// and run the command; otherwise treat everything as a plain cwd run.
fn translate_run(args: &[String], json: bool, explain: bool) -> i32 {
    if args.is_empty() {
        eprintln!("lightr docker: run: missing image/command");
        return 2;
    }

    // Check if args[0] looks like a ref (store lookup)
    let possible_ref = &args[0];
    let cmd_args: &[String] = &args[1..];

    // Try to open the store and check if the ref exists
    let is_known_ref = if let Ok(store) = Store::open(Store::default_root()) {
        store
            .list_refs()
            .ok()
            .is_some_and(|refs| refs.contains(possible_ref))
    } else {
        false
    };

    if is_known_ref && !cmd_args.is_empty() {
        // Hydrate ref to temp dir and run command with it as rootfs
        note_translation(
            "run",
            &[
                "--rootfs",
                possible_ref,
                "--engine",
                "ns",
                "--",
                &cmd_args.join(" "),
            ],
        );
        let command: Vec<String> = cmd_args.to_vec();
        crate::handlers::run::run(
            ".",
            &[],
            &[],
            &command,
            json,
            explain,
            false,
            &[], // publish (Phase 1): docker-translation path publishes nothing
            &[],
            "ns",
            Some(possible_ref),
            false,
            None,
            None,
            &[],
            &[],
            &[],  // env_set untranslated by the docker shim
            None, // env_file
            None, // workdir
            None, // user (WP-RC-USER)
            None, // restart (WP-RC-RESTART)
            &crate::handlers::run::HealthFlags::default(),
        )
    } else if is_known_ref {
        // is_known_ref && cmd_args.is_empty()
        eprintln!(
            "lightr docker: run: ref '{}' found but no command given",
            possible_ref
        );
        2
    } else {
        // ref not found: treat all args as command in cwd
        eprintln!(
            "lightr docker: run: ref '{}' not in store — running as command in cwd",
            possible_ref
        );
        note_translation("run", &["--", &args.join(" ")]);
        let command: Vec<String> = args.to_vec();
        crate::handlers::run::run(
            ".",
            &[],
            &[],
            &command,
            json,
            explain,
            false,
            &[], // publish (Phase 1): docker-translation path publishes nothing
            &[],
            "native",
            None,
            false,
            None,
            None,
            &[],
            &[],
            &[],  // env_set untranslated by the docker shim
            None, // env_file
            None, // workdir
            None, // user (WP-RC-USER)
            None, // restart (WP-RC-RESTART)
            &crate::handlers::run::HealthFlags::default(),
        )
    }
}

// ── docker pull translation ───────────────────────────────────────────────────

fn translate_pull(args: &[String], json: bool) -> i32 {
    if args.is_empty() {
        eprintln!("lightr docker: pull: missing image argument");
        return 2;
    }
    let image = &args[0];
    let ref_name = sanitize_docker_ref(image);

    note_translation("oci", &["pull", image, "--name", &ref_name]);

    crate::handlers::oci::pull_image(image, &ref_name, json)
}

// ── docker images translation ─────────────────────────────────────────────────

fn translate_images(json: bool) -> i32 {
    note_translation("store", &["list-refs"]);

    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    let refs = match store.list_refs() {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    if json {
        let arr = serde_json::to_string(&refs).expect("serialize refs");
        println!("{arr}");
    } else {
        for r in &refs {
            println!("{r}");
        }
    }

    0
}

// ── docker ps translation ─────────────────────────────────────────────────────

fn translate_ps(json: bool) -> i32 {
    note_translation("ps", &[]);
    crate::handlers::ps::run(json)
}

// ── docker inspect translation ────────────────────────────────────────────────

fn translate_inspect(args: &[String], json: bool) -> i32 {
    if args.is_empty() {
        eprintln!("lightr docker: inspect: missing id argument");
        return 2;
    }
    let id = &args[0];
    note_translation("inspect", &[id]);
    // docker inspect always outputs JSON; force json=true regardless of the
    // global --json flag so the single-element array shape is emitted.
    let _ = json; // json is superseded by the always-JSON contract of docker inspect
    crate::handlers::inspect::run(id, true)
}

// ── docker compose translation ────────────────────────────────────────────────

fn translate_compose(args: &[String], json: bool) -> i32 {
    if args.is_empty() {
        eprintln!("lightr docker: compose: missing subcommand");
        return 2;
    }
    match args[0].as_str() {
        "up" => {
            // Parse compose up flags from remaining args
            let rest = &args[1..];
            let mut compose_file = "compose.yml".to_string();
            let mut project: Option<String> = None;
            let mut eager = false;
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
            crate::handlers::compose::up(&compose_file, project.as_deref(), eager, ttl, json)
        }
        "down" => {
            let rest = &args[1..];
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
        sub => {
            eprintln!(
                "lightr docker: compose: unsupported subcommand '{sub}' — supported: up|down"
            );
            2
        }
    }
}

// ── Main dispatch ─────────────────────────────────────────────────────────────

pub fn run(args: &[String], json: bool, explain: bool) -> i32 {
    if args.is_empty() {
        eprintln!(
            "lightr docker: unsupported '' — supported: build|run|pull|images|ps|inspect|compose"
        );
        return 2;
    }

    let subcommand = args[0].as_str();
    let rest = &args[1..];

    match subcommand {
        "build" => translate_build(rest, json, explain),
        "run" => translate_run(rest, json, explain),
        "pull" => translate_pull(rest, json),
        "images" => translate_images(json),
        "ps" => translate_ps(json),
        "inspect" => translate_inspect(rest, json),
        "compose" => translate_compose(rest, json),
        other => {
            eprintln!(
                "lightr docker: unsupported '{other}' — supported: build|run|pull|images|ps|inspect|compose"
            );
            2
        }
    }
}

#[cfg(test)]
mod tests;
