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

pub(super) fn parse_kv(rest: &str) -> Result<(String, String)> {
    if rest.trim().is_empty() {
        return Err(LightrError::InvalidManifest(
            "LABEL/ENV requires a key".to_string(),
        ));
    }
    if let Some((k, v)) = rest.split_once('=') {
        Ok((k.trim().to_string(), v.trim().to_string()))
    } else {
        let (k, v) = rest
            .split_once(|c: char| c.is_ascii_whitespace())
            .map(|(a, b)| (a.trim(), b.trim()))
            .unwrap_or((rest, ""));
        Ok((k.to_string(), v.to_string()))
    }
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
