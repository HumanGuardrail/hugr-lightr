//! FIX #74 — `docker run` flag parser for the compat shim.
//!
//! The shim's `translate_run` previously HARD-CODED nearly every native run
//! parameter to empty/`None`, so `-e`/`-v`/`-p`/`--name`/… silently no-op'd: the
//! user thought the flag took effect, but the native `run` never saw it. This
//! module parses the documented `docker run` flag subset into [`DockerRunArgs`],
//! which the shim then FORWARDS into `handlers::run::run` (which already parses
//! every one of these — prior WPs wired them on the native side).
//!
//! Two invariants, both fail-LOUD (never silent-drop):
//!   • a flag the native side genuinely cannot honor yet ⇒ honest `Err(2)` with
//!     a `not yet supported` message (e.g. `--mount`/`--secret`/`--config`,
//!     whose docker grammars differ from native's `NAME=REF`/`ref:target`);
//!   • an unrecognized `--flag` ⇒ honest `Err(2)`, never misread as the image.
//!
//! On success the FIRST non-flag token is the image and the rest is the command
//! (docker's `run [OPTS] IMAGE [CMD…]` shape).

use lightr_store::Store;

use super::flag_err::unsupported_flag;
use super::note_translation;
use crate::handlers::run::{HealthFlags, RawRcFlags, RawRunFlags};

/// The parsed `docker run …` invocation, lowered to the native run params the
/// shim forwards. Field names mirror `handlers::run::run`'s parameters.
#[derive(Default)]
pub(super) struct DockerRunArgs {
    pub image: Option<String>,
    pub command: Vec<String>,
    pub env_set: Vec<String>,
    pub env_file: Option<String>,
    pub publish: Vec<String>,
    pub publish_all: bool,
    pub workdir: Option<String>,
    pub user: Option<String>,
    pub restart: Option<String>,
    pub stop_signal: Option<String>,
    pub memory: Option<String>,
    pub cpus: Option<String>,
    pub detach: bool,
    pub rc: RawRcFlags,
    pub runflags: RawRunFlags,
}

/// Take the value for a flag that expects one (`--env K=V`), advancing `i`.
/// Supports both the split (`--env K=V`) and `=`-joined (`--env=K=V`) forms —
/// the `=`-joined value is passed pre-extracted in `inline`.
fn take_value(
    args: &[String],
    i: &mut usize,
    inline: Option<&str>,
    flag: &str,
) -> Result<String, i32> {
    if let Some(v) = inline {
        return Ok(v.to_string());
    }
    *i += 1;
    if *i < args.len() {
        Ok(args[*i].clone())
    } else {
        eprintln!("lightr docker: run: flag {flag} requires a value");
        Err(2)
    }
}

/// Honest error for a docker flag whose grammar differs from native's, so a raw
/// forward would misparse rather than no-op. Lists the native equivalent.
fn grammar_mismatch(flag: &str, native_form: &str) -> i32 {
    eprintln!(
        "lightr docker: run: {flag} uses a different value grammar in lightr \
         ({native_form}); docker's form is not yet supported by the shim"
    );
    2
}

/// Parse `docker run [OPTS] IMAGE [CMD…]`. Fail-closed: an unknown flag or a
/// not-yet-supported flag ⇒ `Err(2)` (the honest error is already printed).
pub(super) fn parse(args: &[String]) -> Result<DockerRunArgs, i32> {
    let mut out = DockerRunArgs::default();
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].as_str();
        // Once the image is found, everything after it is the command (docker
        // stops flag parsing at the first positional).
        if out.image.is_some() {
            out.command.push(raw.to_string());
            i += 1;
            continue;
        }
        // Split `--flag=value` into (`--flag`, Some("value")); only for flags.
        let (flag, inline): (&str, Option<&str>) = match raw.split_once('=') {
            Some((f, v)) if raw.starts_with('-') => (f, Some(v)),
            _ => (raw, None),
        };
        if !flag.starts_with('-') {
            // First positional = image; everything after it is the command.
            out.image = Some(raw.to_string());
            i += 1;
            continue;
        }
        match flag {
            "-e" | "--env" => out.env_set.push(take_value(args, &mut i, inline, flag)?),
            "--env-file" => out.env_file = Some(take_value(args, &mut i, inline, flag)?),
            "-v" | "--volume" => out
                .runflags
                .volume
                .push(take_value(args, &mut i, inline, flag)?),
            "-p" | "--publish" => out.publish.push(take_value(args, &mut i, inline, flag)?),
            "-P" | "--publish-all" => out.publish_all = true,
            "-w" | "--workdir" => out.workdir = Some(take_value(args, &mut i, inline, flag)?),
            "-u" | "--user" => out.user = Some(take_value(args, &mut i, inline, flag)?),
            "--name" => out.runflags.name = Some(take_value(args, &mut i, inline, flag)?),
            "--rm" => out.runflags.rm = true,
            "--entrypoint" => {
                out.runflags.entrypoint = Some(take_value(args, &mut i, inline, flag)?)
            }
            "--restart" => out.restart = Some(take_value(args, &mut i, inline, flag)?),
            "--stop-signal" => out.stop_signal = Some(take_value(args, &mut i, inline, flag)?),
            "-m" | "--memory" => out.memory = Some(take_value(args, &mut i, inline, flag)?),
            "--cpus" => out.cpus = Some(take_value(args, &mut i, inline, flag)?),
            "-l" | "--label" => out.rc.label.push(take_value(args, &mut i, inline, flag)?),
            "-d" | "--detach" => out.detach = true,
            // Grammar-mismatch flags: native supports the CONCEPT but with a
            // different value grammar (native `--mount @ref:target`,
            // `--secret/--config NAME=REF`), so forwarding docker's grammar raw
            // would misparse. Honest error rather than a silent misforward.
            "--mount" => {
                return Err(grammar_mismatch(
                    "--mount",
                    "`lightr run --mount @ref:target`",
                ))
            }
            "--secret" => {
                return Err(grammar_mismatch(
                    "--secret",
                    "`lightr run --secret NAME=REF`",
                ))
            }
            "--config" => {
                return Err(grammar_mismatch(
                    "--config",
                    "`lightr run --config NAME=REF`",
                ))
            }
            other => return Err(unsupported_flag("run", other)),
        }
        i += 1;
    }
    Ok(out)
}

/// Resolve the parsed invocation's IMAGE against the store and forward every
/// flag to the native `lightr run`. A known ref hydrates as `--rootfs` under the
/// `ns` engine (the shim's container path); an unknown ref falls back to a plain
/// cwd run under `native` (image token becomes the first command word). Both
/// paths carry the FULL forwarded flag set — no more silent no-ops (FIX #74).
pub(super) fn forward(parsed: DockerRunArgs, json: bool, explain: bool) -> i32 {
    let image = match parsed.image {
        Some(img) => img,
        None => {
            eprintln!("lightr docker: run: missing image");
            return 2;
        }
    };

    // Is the image a known store ref? (best-effort — a store-open failure just
    // means "treat as a cwd command", exactly as before).
    let is_known_ref = Store::open(Store::default_root())
        .ok()
        .and_then(|s| s.list_refs().ok())
        .is_some_and(|refs| refs.contains(&image));

    let (engine, rootfs, command): (&str, Option<&str>, Vec<String>) = if is_known_ref {
        // Known ref ⇒ hydrate it as the rootfs (ns container path). The command
        // is whatever followed the image (may be empty ⇒ image default).
        ("ns", Some(image.as_str()), parsed.command.clone())
    } else {
        // Unknown ref ⇒ plain cwd run; the image token is the first command word.
        eprintln!("lightr docker: run: ref '{image}' not in store — running as command in cwd");
        let mut cmd = vec![image.clone()];
        cmd.extend(parsed.command.iter().cloned());
        ("native", None, cmd)
    };

    note_translation("run", &["--engine", engine, "--", &command.join(" ")]);

    #[allow(clippy::too_many_arguments)]
    crate::handlers::run::run(
        ".",
        &[],
        &[],
        &command,
        json,
        explain,
        parsed.detach,
        &parsed.publish,
        parsed.publish_all,
        &[], // mounts: docker `--mount` is grammar-mismatch (honest-errored above)
        engine,
        rootfs,
        false,
        parsed.memory.as_deref(),
        parsed.cpus.as_deref(),
        &[], // secrets: docker `--secret` is grammar-mismatch (honest-errored above)
        &[], // configs: docker `--config` is grammar-mismatch (honest-errored above)
        &parsed.env_set,
        parsed.env_file.as_deref(),
        parsed.workdir.as_deref(),
        parsed.user.as_deref(),
        parsed.restart.as_deref(),
        parsed.stop_signal.as_deref(),
        &HealthFlags::default(),
        parsed.rc,
        parsed.runflags,
    )
}

#[cfg(test)]
#[path = "run_args_tests.rs"]
mod tests;
