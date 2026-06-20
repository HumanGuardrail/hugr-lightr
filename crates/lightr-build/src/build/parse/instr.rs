//! Per-instruction parsing helpers (WP-DF-01).
//!
//! Each `parse_*` turns an instruction's argument tail into a structured
//! [`Instr`]. Flags are parsed into fields; no `${VAR}` interpolation. The
//! dispatcher lives in `super` (`parse/mod.rs`); ONBUILD recurses back into it.

use super::ast::{CmdForm, Healthcheck, HealthcheckOpts, Instr};
use super::flags::{split_flags, take_flag};
use super::parse_instruction;
use lightr_core::{LightrError, Result};

pub(super) fn parse_from(rest: &str) -> Result<Instr> {
    let (platform, rest) = take_flag(rest, "platform");
    let toks: Vec<&str> = rest.split_ascii_whitespace().collect();
    if toks.is_empty() {
        return Err(LightrError::InvalidManifest(
            "FROM requires an image reference".to_string(),
        ));
    }
    let image_ref = toks[0].to_string();
    let mut stage = None;
    if toks.len() >= 3 && toks[1].eq_ignore_ascii_case("AS") {
        stage = Some(toks[2].to_string());
    } else if toks.len() != 1 {
        // Only `FROM img` (len 1) and `FROM img AS name` (len 3) are valid;
        // anything else is malformed.
        return Err(LightrError::InvalidManifest(format!(
            "FROM: expected `<image> [AS <name>]`, got: {rest}"
        )));
    }
    Ok(Instr::From {
        image_ref,
        platform,
        stage,
    })
}

pub(super) fn parse_add(rest: &str) -> Result<Instr> {
    let (flags, positional) = split_flags(rest);
    let chown = find_flag(&flags, "chown");
    let chmod = find_flag(&flags, "chmod");
    let (src, dest) = src_dest(&positional, "ADD")?;
    Ok(Instr::Add {
        src,
        dest,
        chown,
        chmod,
    })
}

pub(super) fn parse_copy(rest: &str) -> Result<Instr> {
    let (flags, positional) = split_flags(rest);
    let from = find_flag(&flags, "from");
    let chown = find_flag(&flags, "chown");
    let chmod = find_flag(&flags, "chmod");
    let (src, dest) = src_dest(&positional, "COPY")?;
    Ok(Instr::Copy {
        src,
        dest,
        from,
        chown,
        chmod,
    })
}

pub(super) fn parse_arg(rest: &str) -> Instr {
    if let Some((name, default)) = rest.split_once('=') {
        Instr::Arg {
            name: name.trim().to_string(),
            default: Some(default.trim().to_string()),
        }
    } else {
        Instr::Arg {
            name: rest.trim().to_string(),
            default: None,
        }
    }
}

pub(super) fn parse_onbuild(rest: &str) -> Result<Instr> {
    let inner_verb = rest
        .split_once(|c: char| c.is_ascii_whitespace())
        .map(|(k, _)| k)
        .unwrap_or(rest)
        .to_ascii_uppercase();
    // Docker forbids ONBUILD chaining and ONBUILD FROM / MAINTAINER.
    if matches!(inner_verb.as_str(), "ONBUILD" | "FROM" | "MAINTAINER") {
        return Err(LightrError::InvalidManifest(format!(
            "ONBUILD {inner_verb} is not allowed"
        )));
    }
    let inner = parse_instruction(rest)?;
    Ok(Instr::Onbuild {
        instr: Box::new(inner),
    })
}

pub(super) fn parse_shell(rest: &str) -> Result<Instr> {
    let t = rest.trim();
    let shell = serde_json::from_str::<Vec<String>>(t).map_err(|_| {
        LightrError::InvalidManifest(format!("SHELL requires JSON-array exec form, got: {rest}"))
    })?;
    if shell.is_empty() {
        return Err(LightrError::InvalidManifest(
            "SHELL requires a non-empty JSON array".to_string(),
        ));
    }
    Ok(Instr::Shell { shell })
}

pub(super) fn parse_healthcheck(rest: &str) -> Result<Instr> {
    let t = rest.trim();
    if t.eq_ignore_ascii_case("NONE") {
        return Ok(Instr::Healthcheck {
            check: Healthcheck::None,
        });
    }
    let (flags, _) = split_flags(t);
    let mut opts = HealthcheckOpts::default();
    for (k, v) in &flags {
        match k.as_str() {
            "interval" => opts.interval = Some(v.clone()),
            "timeout" => opts.timeout = Some(v.clone()),
            "start-period" => opts.start_period = Some(v.clone()),
            "start-interval" => opts.start_interval = Some(v.clone()),
            "retries" => opts.retries = Some(v.clone()),
            other => {
                return Err(LightrError::InvalidManifest(format!(
                    "HEALTHCHECK: unknown flag --{other}"
                )));
            }
        }
    }
    // After flags, the body must be `CMD <command>`.
    let after_flags = strip_leading_flags(t);
    let (kw, cmd_rest) = after_flags
        .split_once(|c: char| c.is_ascii_whitespace())
        .map(|(k, r)| (k, r.trim()))
        .unwrap_or((after_flags.as_str(), ""));
    if !kw.eq_ignore_ascii_case("CMD") {
        return Err(LightrError::InvalidManifest(format!(
            "HEALTHCHECK: expected CMD or NONE, got: {rest}"
        )));
    }
    Ok(Instr::Healthcheck {
        check: Healthcheck::Cmd {
            opts,
            cmd: cmd_form(cmd_rest),
        },
    })
}

/// Parse the ENV/LABEL argument tail into one OR MANY `(key, value)` pairs
/// (WP-DF-05), Docker-faithful:
///
/// - **Legacy form** `ENV KEY value` — the FIRST token has no `=`, so the
///   WHOLE remaining tail (after the key) is a single value (spaces kept,
///   un-quoted). Preserves the pre-WP-DF-05 single-pair behavior exactly.
/// - **Multi-pair form** `ENV A=1 B=2 C=3` — the first token contains `=`, so
///   every whitespace-separated `KEY=VALUE` token is one pair.
/// - **Quoting** `ENV A="x y" B='z'` — single/double quotes group a value that
///   contains spaces; the quotes are stripped from the stored value. Inside a
///   double-quoted value `\"` and `\\` are escapes (Docker); single quotes are
///   literal. No `${VAR}` interpolation here — that happens at exec time.
///
/// A multi-pair token without `=` is malformed (fail-closed); an empty tail is
/// an error (a key is required).
pub(super) fn parse_kv_pairs(rest: &str) -> Result<Vec<(String, String)>> {
    let t = rest.trim();
    if t.is_empty() {
        return Err(LightrError::InvalidManifest(
            "LABEL/ENV requires a key".to_string(),
        ));
    }

    let tokens = tokenize_quoted(t)?;
    // `tokenize_quoted` yields at least one token for a non-empty input. The
    // first token decides the form: legacy (no `=`) vs multi-pair (`=` present).
    let first_has_eq = tokens
        .first()
        .map(|tok| matches!(tok.find('='), Some(i) if i > 0))
        .unwrap_or(false);

    if !first_has_eq {
        // Legacy `KEY value`: split the RAW (un-tokenized) tail on the first
        // whitespace so the value keeps spaces verbatim and is NOT subject to
        // per-token quote handling (Docker's legacy value = rest of the line).
        let (k, v) = t
            .split_once(|c: char| c.is_ascii_whitespace())
            .map(|(a, b)| (a.trim(), b.trim()))
            .unwrap_or((t, ""));
        return Ok(vec![(k.to_string(), v.to_string())]);
    }

    // Multi-pair `KEY=VALUE [KEY2=VALUE2 ...]`: every token must contain `=`.
    let mut pairs = Vec::with_capacity(tokens.len());
    for tok in &tokens {
        let Some((k, v)) = tok.split_once('=') else {
            return Err(LightrError::InvalidManifest(format!(
                "ENV/LABEL: expected KEY=VALUE, got: {tok}"
            )));
        };
        let key = k.trim();
        if key.is_empty() {
            return Err(LightrError::InvalidManifest(
                "ENV/LABEL: empty key in KEY=VALUE".to_string(),
            ));
        }
        pairs.push((key.to_string(), v.to_string()));
    }
    Ok(pairs)
}

/// Split a string into tokens on UNQUOTED whitespace, honoring `'…'` and `"…"`
/// quoting (quotes removed from the result). Inside double quotes `\"` and `\\`
/// are escapes; single quotes are literal (POSIX/Docker). Quotes that abut
/// other text join into the same token (`A="x y"` is one token). An
/// unterminated quote is fail-closed.
fn tokenize_quoted(s: &str) -> Result<Vec<String>> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has_tok = false;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_ascii_whitespace() => {
                if has_tok {
                    tokens.push(std::mem::take(&mut cur));
                    has_tok = false;
                }
            }
            '\'' => {
                has_tok = true;
                let mut closed = false;
                for q in chars.by_ref() {
                    if q == '\'' {
                        closed = true;
                        break;
                    }
                    cur.push(q);
                }
                if !closed {
                    return Err(LightrError::InvalidManifest(format!(
                        "ENV/LABEL: unterminated single quote in: {s}"
                    )));
                }
            }
            '"' => {
                has_tok = true;
                let mut closed = false;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => match chars.next() {
                            Some(e @ ('"' | '\\')) => cur.push(e),
                            Some(other) => {
                                cur.push('\\');
                                cur.push(other);
                            }
                            None => break,
                        },
                        other => cur.push(other),
                    }
                }
                if !closed {
                    return Err(LightrError::InvalidManifest(format!(
                        "ENV/LABEL: unterminated double quote in: {s}"
                    )));
                }
            }
            other => {
                has_tok = true;
                cur.push(other);
            }
        }
    }
    if has_tok {
        tokens.push(cur);
    }
    Ok(tokens)
}

/// VOLUME: JSON array form OR whitespace-separated paths.
pub(super) fn parse_paths(rest: &str) -> Vec<String> {
    let t = rest.trim();
    if t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Vec<String>>(t) {
            return v;
        }
    }
    t.split_ascii_whitespace().map(str::to_string).collect()
}

pub(super) fn non_empty(rest: &str, verb: &str) -> Result<String> {
    let t = rest.trim();
    if t.is_empty() {
        return Err(LightrError::InvalidManifest(format!(
            "{verb} requires an argument"
        )));
    }
    Ok(t.to_string())
}

/// Structured exec-vs-shell form for RUN/CMD/ENTRYPOINT/HEALTHCHECK CMD.
pub(super) fn cmd_form(rest: &str) -> CmdForm {
    let t = rest.trim();
    if t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Vec<String>>(t) {
            return CmdForm::Exec(v);
        }
    }
    CmdForm::Shell(t.to_string())
}

/// Resolved exec argv for RUN/CMD/ENTRYPOINT: exec form verbatim, shell form
/// wrapped as `["/bin/sh","-c",<rest>]` (preserves the existing exec.rs path).
pub(super) fn cmd_argv(rest: &str) -> Vec<String> {
    match cmd_form(rest) {
        CmdForm::Exec(v) => v,
        CmdForm::Shell(s) => vec!["/bin/sh".to_string(), "-c".to_string(), s],
    }
}

fn find_flag(flags: &[(String, String)], name: &str) -> Option<String> {
    flags
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// Drop a run of leading `--flag[=value]` tokens, returning the remainder.
fn strip_leading_flags(s: &str) -> String {
    let mut rest = s.trim();
    while let Some(stripped) = rest.strip_prefix("--") {
        let end = stripped.find(char::is_whitespace).unwrap_or(stripped.len());
        rest = stripped[end..].trim_start();
    }
    rest.to_string()
}

/// Resolve the (src.., dest) split for ADD/COPY positional args.
fn src_dest(positional: &[String], verb: &str) -> Result<(Vec<String>, String)> {
    if positional.len() < 2 {
        return Err(LightrError::InvalidManifest(format!(
            "{verb} requires at least src dest"
        )));
    }
    let dest = positional.last().unwrap().clone();
    let src = positional[..positional.len() - 1].to_vec();
    Ok((src, dest))
}
