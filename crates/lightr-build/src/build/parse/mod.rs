//! Dockerfile parsing — the full 18-instruction parser (WP-DF-01).
//!
//! Produces a structured [`ast::Instr`] AST for every Dockerfile instruction.
//! It parses into structured form (flags into fields, JSON-array vs shell form,
//! parser directives); it does NOT execute the instructions and does NOT
//! interpolate `${VAR}` (WP-DF-02 / R-VARENG consumes this AST later).
//!
//! Rules:
//! - Parser directives (`# syntax=`, `# escape=`) are recognized only in the
//!   leading comment block, before any instruction or non-directive comment.
//! - Lines ending with the active escape char (`\` default, or `` ` `` when
//!   `# escape=` says so) continue onto the next line.
//! - Comments (`#` after leading whitespace) and blank logical lines are skipped.
//! - The instruction verb is case-insensitive.
//! - A genuinely unknown verb is a parse ERROR (fail-closed, honest).

mod ast;
mod flags;
mod instr;

pub use ast::{BuildStep, CmdForm, Directives, Healthcheck, HealthcheckOpts, Instr};

use instr::{
    cmd_argv, cmd_form, non_empty, parse_add, parse_arg, parse_copy, parse_from, parse_healthcheck,
    parse_kv, parse_onbuild, parse_paths, parse_shell,
};
use lightr_core::{LightrError, Result};

/// The 18 recognized Dockerfile instruction verbs (plus MAINTAINER, the
/// deprecated 19th Docker still accepts — kept out of the AST but tolerated as
/// a no-op LABEL-less form is NOT in scope; MAINTAINER is treated as unknown).
/// We recognize exactly the 18 the contract names.
const KNOWN_VERBS: [&str; 18] = [
    "FROM",
    "RUN",
    "CMD",
    "LABEL",
    "EXPOSE",
    "ENV",
    "ADD",
    "COPY",
    "ENTRYPOINT",
    "VOLUME",
    "USER",
    "WORKDIR",
    "ARG",
    "ONBUILD",
    "STOPSIGNAL",
    "HEALTHCHECK",
    "SHELL",
    "MAINTAINER",
];

/// Parse a Dockerfile, returning only the instruction steps (back-compat API).
///
/// Equivalent to [`parse_dockerfile_full`] discarding the directives.
pub fn parse_dockerfile(text: &str) -> Result<Vec<BuildStep>> {
    Ok(parse_dockerfile_full(text)?.1)
}

/// Parse a Dockerfile into its parser directives + instruction steps.
pub fn parse_dockerfile_full(text: &str) -> Result<(Directives, Vec<BuildStep>)> {
    let (directives, body) = scan_directives(text);
    let escape = directives.escape.unwrap_or('\\');
    let logical = join_continuations(&body, escape);

    let mut steps = Vec::new();
    for line in logical {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let instr = parse_instruction(trimmed)?;
        steps.push(BuildStep {
            instr,
            raw: trimmed.to_string(),
        });
    }
    Ok((directives, steps))
}

/// Scan the leading comment block for `# syntax=` / `# escape=` directives.
///
/// Per Docker, a directive must appear before any builder instruction or any
/// non-directive comment/blank-significant line; the first non-comment line (or
/// a comment that is not a directive) ends the directive block. The returned
/// `String` is the full original text — directives are comments, so leaving
/// them in the body is harmless (the comment-skip drops them); we return the
/// original text unchanged to keep line semantics intact.
fn scan_directives(text: &str) -> (Directives, String) {
    let mut d = Directives::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            // A blank line ends the directive block in Docker.
            break;
        }
        let Some(comment) = line.strip_prefix('#') else {
            break; // first instruction ends the block
        };
        let body = comment.trim();
        let Some((k, v)) = body.split_once('=') else {
            break; // a non-directive comment ends the block
        };
        match k.trim().to_ascii_lowercase().as_str() {
            "syntax" => d.syntax = Some(v.trim().to_string()),
            "escape" => d.escape = v.trim().chars().next(),
            _ => break,
        }
    }
    (d, text.to_string())
}

/// Join continuation lines honoring the active escape char. A line whose
/// (right-trimmed) content ends in the escape char continues onto the next.
/// Comment lines do not participate in continuation (Docker drops them whole).
fn join_continuations(text: &str, escape: char) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut continuing = false;
    for raw in text.lines() {
        // A comment line inside a continuation is dropped (Docker behavior),
        // but a standalone comment is preserved as its own logical line so the
        // skip-step recognizes it.
        let is_comment = raw.trim_start().starts_with('#');
        if continuing && is_comment {
            continue;
        }
        let trimmed_end = raw.trim_end();
        if trimmed_end.ends_with(escape) && !is_comment {
            let body = &trimmed_end[..trimmed_end.len() - escape.len_utf8()];
            current.push_str(body);
            current.push(' ');
            continuing = true;
        } else {
            current.push_str(raw);
            out.push(std::mem::take(&mut current));
            continuing = false;
        }
    }
    if continuing && !current.is_empty() {
        out.push(current);
    }
    out
}

/// Parse a single logical (continuation-joined, non-comment) line into an
/// `Instr`. The verb is the first whitespace-delimited token.
fn parse_instruction(line: &str) -> Result<Instr> {
    let (verb, rest) = line
        .split_once(|c: char| c.is_ascii_whitespace())
        .map(|(k, r)| (k, r.trim()))
        .unwrap_or((line, ""));
    let upper = verb.to_ascii_uppercase();
    if !KNOWN_VERBS.contains(&upper.as_str()) {
        return Err(LightrError::InvalidManifest(format!(
            "unsupported instruction: {verb}"
        )));
    }
    match upper.as_str() {
        "FROM" => parse_from(rest),
        "RUN" => Ok(Instr::Run {
            argv: cmd_argv(rest),
            form: cmd_form(rest),
        }),
        "CMD" => Ok(Instr::Cmd {
            argv: cmd_argv(rest),
            form: cmd_form(rest),
        }),
        "ENTRYPOINT" => Ok(Instr::Entrypoint {
            argv: cmd_argv(rest),
            form: cmd_form(rest),
        }),
        "LABEL" => parse_kv(rest).map(|(key, val)| Instr::Label { key, val }),
        "ENV" => parse_kv(rest).map(|(key, val)| Instr::Env { key, val }),
        "EXPOSE" => Ok(Instr::Expose {
            ports: rest.split_ascii_whitespace().map(str::to_string).collect(),
        }),
        "ADD" => parse_add(rest),
        "COPY" => parse_copy(rest),
        "VOLUME" => Ok(Instr::Volume {
            paths: parse_paths(rest),
        }),
        "USER" => non_empty(rest, "USER").map(|user| Instr::User { user }),
        "WORKDIR" => non_empty(rest, "WORKDIR").map(|path| Instr::Workdir { path }),
        "ARG" => Ok(parse_arg(rest)),
        "ONBUILD" => parse_onbuild(rest),
        "STOPSIGNAL" => non_empty(rest, "STOPSIGNAL").map(|signal| Instr::Stopsignal { signal }),
        "HEALTHCHECK" => parse_healthcheck(rest),
        "SHELL" => parse_shell(rest),
        // MAINTAINER is in KNOWN_VERBS only to reject it explicitly (deprecated,
        // not in the 18-instruction AST). Treat as unsupported, fail-closed.
        "MAINTAINER" => Err(LightrError::InvalidManifest(
            "unsupported instruction: MAINTAINER (deprecated; use LABEL)".to_string(),
        )),
        // Unreachable: KNOWN_VERBS gate above covers every arm.
        other => Err(LightrError::InvalidManifest(format!(
            "unsupported instruction: {other}"
        ))),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests_instr.rs"]
mod tests_instr;
