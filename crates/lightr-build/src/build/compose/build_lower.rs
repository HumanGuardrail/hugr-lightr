//! WP-E: lower a service's `build:` ([`BuildSpec`]) into the runtime
//! [`ServiceBuild`] the up-path feeds to `build_target`.
//!
//! Normalizes both compose shapes:
//!   * SHORT (`build: ./app`) ⇒ context = the string, dockerfile = `Dockerfile`.
//!   * LONG (`build: { context, dockerfile, args, target }`) ⇒ the fields, with
//!     Docker defaults (`dockerfile = Dockerfile`).
//!
//! The context is resolved against `base_dir` (the compose file's directory)
//! when known — matching Docker, which resolves a relative build context
//! against the compose file's location. When `base_dir` is `None` (the legacy
//! `parse_compose` path that carries no directory), a relative context is left
//! as-declared and resolved against the process CWD at build time — the SAME
//! fallback `lower_env`'s `env_file` uses. An ABSOLUTE context is unaffected by
//! either path.
//!
//! `dockerfile` is left RELATIVE to the context (Docker's rule — the build
//! entrypoint joins `context.join(dockerfile)`); only the context is anchored.
//!
//! Build-args are lowered to ordered `(KEY, value)` pairs here, resolving a
//! bare `KEY` (no value) through the process environment ONCE, so the up-path
//! needs no env access (parallel-safe — the env read is at lowering, the same
//! place compose already reads env for `${VAR}` interpolation).
use std::path::Path;

use lightr_core::{LightrError, Result};

use super::build_spec::{BuildArgs, BuildLong, BuildSpec, ServiceBuild};

/// Default Dockerfile name (relative to the context), Docker's rule.
const DEFAULT_DOCKERFILE: &str = "Dockerfile";

/// Lower a service's `build:` against `base_dir` (the compose file's directory,
/// when known). Returns `Ok(None)` when the service declares no `build:`.
///
/// Reads the process env to resolve a bare-`KEY` build-arg; inject a custom
/// `env_lookup` in tests for parallel-safety.
pub(crate) fn lower_build(
    build: Option<&BuildSpec>,
    base_dir: Option<&Path>,
) -> Result<Option<ServiceBuild>> {
    lower_build_with_env(build, base_dir, &|k| std::env::var(k).ok())
}

/// Pure core of [`lower_build`]: `env_lookup` resolves a bare-`KEY` build-arg so
/// tests stay parallel-safe (no process-global env read).
pub(crate) fn lower_build_with_env(
    build: Option<&BuildSpec>,
    base_dir: Option<&Path>,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<ServiceBuild>> {
    let Some(build) = build else {
        return Ok(None);
    };
    // Both shapes produce a `ServiceBuild` with the context AS-DECLARED; the
    // emptiness check + base-dir anchoring are applied uniformly below.
    let mut sb = match build {
        BuildSpec::Short(ctx) => ServiceBuild {
            context: ctx.clone(),
            dockerfile: DEFAULT_DOCKERFILE.to_string(),
            args: Vec::new(),
            target: None,
        },
        BuildSpec::Long(long) => lower_long(long, env_lookup)?,
    };
    if sb.context.trim().is_empty() {
        return Err(LightrError::InvalidManifest(
            "compose build: context is empty (a `build:` needs a context directory)".to_string(),
        ));
    }
    sb.context = anchor_context(&sb.context, base_dir);
    Ok(Some(sb))
}

/// Lower the long `build:` mapping into a [`ServiceBuild`] (context as-declared;
/// the caller anchors + validates it).
fn lower_long(
    long: &BuildLong,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<ServiceBuild> {
    let context = long.context.clone().ok_or_else(|| {
        LightrError::InvalidManifest(
            "compose build: long form requires a `context:` key".to_string(),
        )
    })?;
    Ok(ServiceBuild {
        context,
        dockerfile: long
            .dockerfile
            .clone()
            .unwrap_or_else(|| DEFAULT_DOCKERFILE.to_string()),
        args: lower_args(long.args.as_ref(), env_lookup),
        target: long.target.clone(),
    })
}

/// Lower the `args:` block (map or list) to ordered `(KEY, value)` pairs. A bare
/// `KEY` (map null value OR a list entry with no `=`) resolves through
/// `env_lookup`; an unset bare key is DROPPED (Docker passes nothing for it),
/// matching `docker build --build-arg KEY` with no host value.
fn lower_args(
    args: Option<&BuildArgs>,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match args {
        None => {}
        Some(BuildArgs::Map(map)) => {
            for (k, v) in map {
                match v {
                    Some(val) => out.push((k.clone(), value_to_string(val))),
                    None => push_env(&mut out, k, env_lookup),
                }
            }
        }
        Some(BuildArgs::List(items)) => {
            for item in items {
                match item.split_once('=') {
                    Some((k, v)) => out.push((k.to_string(), v.to_string())),
                    None => push_env(&mut out, item, env_lookup),
                }
            }
        }
    }
    out
}

/// Push `(key, env-value)` when the process env defines `key`; drop it otherwise.
fn push_env(
    out: &mut Vec<(String, String)>,
    key: &str,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) {
    if let Some(v) = env_lookup(key) {
        out.push((key.to_string(), v));
    }
}

/// Render a scalar YAML build-arg value as a string. Bool/number scalars become
/// their textual form (Docker stringifies build-args); a non-scalar is rendered
/// empty (it cannot be a meaningful ARG value).
fn value_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Anchor a relative context against `base_dir` when known; leave an absolute
/// context (or a relative one with no base dir) untouched.
fn anchor_context(context: &str, base_dir: Option<&Path>) -> String {
    let p = Path::new(context);
    if p.is_absolute() {
        return context.to_string();
    }
    match base_dir {
        Some(dir) => dir.join(p).to_string_lossy().into_owned(),
        None => context.to_string(),
    }
}

#[cfg(test)]
#[path = "build_lower_tests.rs"]
mod tests;
