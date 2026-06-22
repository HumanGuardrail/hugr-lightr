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

mod compose;
mod flag_err;
mod run_args;

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

pub(super) fn note_translation(lightr_verb: &str, lightr_args: &[&str]) {
    let args_str = lightr_args.join(" ");
    if args_str.is_empty() {
        eprintln!("lightr docker: → lightr {lightr_verb}");
    } else {
        eprintln!("lightr docker: → lightr {lightr_verb} {args_str}");
    }
}

// ── docker build translation ──────────────────────────────────────────────────

/// Parse `docker build [-t TAG] [-f FILE] [--build-arg N=V]… [--target STAGE]
/// <CTX>` and dispatch to `handlers::build::run`.
///
/// FIX #74: `--build-arg` and `--target` are now FORWARDED (the native
/// `lightr build` parses both — prior WPs). The catch-all that misread an
/// unrecognized `--flag` as the context dir is replaced by an HONEST error, so
/// a typo'd flag can never be silently swallowed as the positional.
fn translate_build(args: &[String], json: bool, explain: bool) -> i32 {
    let mut tag: Option<String> = None;
    let mut dockerfile: Option<String> = None;
    let mut context: Option<String> = None;
    let mut build_args: Vec<String> = Vec::new();
    let mut target: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let raw = args[i].as_str();
        // Split `--flag=value` once so both `--target s` and `--target=s` work.
        let (flag, inline): (&str, Option<&str>) = match raw.split_once('=') {
            Some((f, v)) if raw.starts_with("--") => (f, Some(v)),
            _ => (raw, None),
        };
        match flag {
            "-t" | "--tag" => match build_take(args, &mut i, inline) {
                Some(v) => tag = Some(v),
                None => return build_missing_value(flag),
            },
            "-f" | "--file" => match build_take(args, &mut i, inline) {
                Some(v) => dockerfile = Some(v),
                None => return build_missing_value(flag),
            },
            "--build-arg" => match build_take(args, &mut i, inline) {
                Some(v) => build_args.push(v),
                None => return build_missing_value(flag),
            },
            "--target" => match build_take(args, &mut i, inline) {
                Some(v) => target = Some(v),
                None => return build_missing_value(flag),
            },
            // CATCH-ALL TRAP FIX: an unrecognized FLAG is an honest error, never
            // misread as the context dir (which silently dropped it before).
            f if f.starts_with('-') => return flag_err::unsupported_flag("build", f),
            // A bare positional is the context dir (docker's `build [OPTS] CTX`).
            _ => context = Some(raw.to_string()),
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

    // FIX #74: forward `--build-arg` + `--target` to the native build, which has
    // parsed both since the WP-C follow-up. No longer silent-dropped.
    crate::handlers::build::run(
        &ctx,
        dockerfile.as_deref(),
        &name,
        "native",
        &build_args,
        json,
        explain,
        target.as_deref(),
    )
}

/// Read the value for a build flag (split or `=`-joined). `None` ⇒ no value
/// available (the caller emits an honest missing-value error).
fn build_take(args: &[String], i: &mut usize, inline: Option<&str>) -> Option<String> {
    if let Some(v) = inline {
        return Some(v.to_string());
    }
    *i += 1;
    args.get(*i).cloned()
}

/// Honest "flag requires a value" error for `docker build` (exit 2).
fn build_missing_value(flag: &str) -> i32 {
    eprintln!("lightr docker: build: flag {flag} requires a value");
    2
}

// ── docker run translation ────────────────────────────────────────────────────

/// `docker run [OPTS] IMAGE [CMD…]` → forward every documented flag to the
/// native `lightr run` (which already parses them — FIX #74). The IMAGE token is
/// resolved against the store: a known ref hydrates as `--rootfs` (ns engine), an
/// unknown one falls back to a plain cwd run. Flag parsing + the honest-error
/// rules (grammar-mismatch / unrecognized) live in `run_args`; this fn owns only
/// the image/store resolution + the forward.
fn translate_run(args: &[String], json: bool, explain: bool) -> i32 {
    if args.is_empty() {
        eprintln!("lightr docker: run: missing image/command");
        return 2;
    }

    // Parse the docker flag subset; an unknown/unsupported flag is an honest
    // exit 2 already printed by the parser (never a silent drop).
    let parsed = match run_args::parse(args) {
        Ok(p) => p,
        Err(code) => return code,
    };
    run_args::forward(parsed, json, explain)
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
// Moved to `compose.rs` (godfile headroom); see `compose::translate_compose`.

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
        "compose" => compose::translate_compose(rest, json),
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
