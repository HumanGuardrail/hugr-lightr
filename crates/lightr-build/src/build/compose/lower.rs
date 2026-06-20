//! Lower the serde compose-spec model (`spec.rs`) to the runtime `Compose`
//! type that up/down/supervise consume.
//!
//! Behavior-preserving: this reproduces, field for field, what the legacy
//! hand-rolled state machine produced, so downstream is byte-for-byte
//! unaffected. Richer spec fields (build, deploy, profiles, ...) are not
//! representable in `Compose` yet and are simply not lowered (CMP-P1/P2).
use std::path::Path;

use lightr_core::{LightrError, Result};

use super::envfile::read_env_file;
use super::model::{empty_service, parse_duration_secs, Compose, Service};
use super::ports::{parse_ports, ParsedPort};
use super::spec::{ComposeSpec, Environment, Healthcheck, ServiceDef, StringOrList};

/// Lower a deserialized spec into the runtime `Compose`, preserving service
/// declaration order.
///
/// Behavior-preserving: identical to today's path. Equivalent to
/// [`lower_with_base_dir`] with `base_dir == None`, so services that declare an
/// `env_file` (which needs the compose file's directory to resolve relative
/// paths) lower WITHOUT the file contributions on this entry. The base-dir-
/// aware entry is used by the handler-side call site that knows the compose
/// file's directory.
pub(crate) fn lower(spec: ComposeSpec) -> Result<Compose> {
    lower_with_base_dir(spec, None)
}

/// Lower a deserialized spec, resolving each service's `env_file` paths relative
/// to `base_dir` (the compose file's directory). When `base_dir` is `None`,
/// `env_file` entries are not read (no directory to resolve against) and the
/// result equals the legacy [`lower`] path.
///
/// Precedence (compose rule): env_file contents are folded FIRST (listed order;
/// later files override earlier), then the inline `environment` block is applied
/// ON TOP (inline overrides file). A required-but-missing env_file is an honest
/// error. Bare `KEY` lines pass through the process env.
pub(crate) fn lower_with_base_dir(spec: ComposeSpec, base_dir: Option<&Path>) -> Result<Compose> {
    let mut services = Vec::with_capacity(spec.services.len());
    for (name, def) in spec.services {
        services.push(lower_service(name, def, base_dir)?);
    }
    Ok(Compose { services })
}

fn lower_service(name: String, def: ServiceDef, base_dir: Option<&Path>) -> Result<Service> {
    let mut svc = empty_service(name);

    if let Some(image) = def.image {
        svc.image_ref = image;
    }

    svc.command = def.command.map(lower_command);

    svc.env = lower_env(def.env_file.as_ref(), def.environment, base_dir)?;

    svc.ports = lower_ports(&def.ports)?;

    svc.eager = def.x_lightr_eager.unwrap_or(false);

    svc.secrets = lower_pairs(&def.secrets);
    svc.configs = lower_pairs(&def.configs);

    svc.healthcheck = lower_healthcheck(def.healthcheck)?;

    Ok(svc)
}

/// `command`: a bare string becomes a `/bin/sh -c` wrapper (legacy semantics);
/// a list is taken as the argv as-is.
fn lower_command(c: StringOrList) -> Vec<String> {
    match c {
        StringOrList::String(s) => {
            vec!["/bin/sh".to_string(), "-c".to_string(), s]
        }
        StringOrList::List(v) => v,
    }
}

/// Fold a service's env sources into the final ordered `(K, V)` list:
/// each `env_file` (in listed order; later files override earlier) FIRST, then
/// the inline `environment` block ON TOP (inline overrides file). Paths are
/// resolved against `base_dir` (the compose file's directory).
///
/// Behavior-preserving: with NO `env_file` declared the result is byte-for-byte
/// the legacy `lower_environment(env)` (or an empty vec when `environment` is
/// also absent) — no override-collapsing is applied on that path.
fn lower_env(
    env_file: Option<&StringOrList>,
    environment: Option<Environment>,
    base_dir: Option<&Path>,
) -> Result<Vec<(String, String)>> {
    // No env_file → exactly today's behavior. `base_dir == None` ALSO falls
    // back to today's behavior: env_file resolution needs the compose file's
    // directory, and the legacy `parse_compose` path (no base dir) parsed an
    // `env_file` field and IGNORED it — so we preserve that, fail-closed-free,
    // rather than guessing a directory.
    let (Some(env_file), Some(dir)) = (env_file, base_dir) else {
        return Ok(environment.map(lower_environment).unwrap_or_default());
    };

    let mut env: Vec<(String, String)> = Vec::new();
    for rel in string_or_list_paths(env_file) {
        let path = dir.join(&rel);
        let pairs = read_env_file(&path, &|k| std::env::var(k).ok())?;
        for (k, v) in pairs {
            upsert(&mut env, k, v);
        }
    }
    // Inline `environment` block applied ON TOP (overrides file).
    if let Some(environment) = environment {
        for (k, v) in lower_environment(environment) {
            upsert(&mut env, k, v);
        }
    }
    Ok(env)
}

/// Insert `(k, v)` with last-wins override semantics, preserving the position of
/// an existing key (compose treats env as a keyed set; the later source wins the
/// value).
fn upsert(env: &mut Vec<(String, String)>, k: String, v: String) {
    if let Some(slot) = env.iter_mut().find(|(ek, _)| *ek == k) {
        slot.1 = v;
    } else {
        env.push((k, v));
    }
}

/// The `env_file` field as an ordered list of paths (string form = one path).
fn string_or_list_paths(env_file: &StringOrList) -> Vec<String> {
    match env_file {
        StringOrList::String(s) => vec![s.clone()],
        StringOrList::List(v) => v.clone(),
    }
}

/// `environment`: list form is `K=V` (value may contain further `=`); map form
/// is `K: V`. The legacy parser SKIPPED map entries with an empty value, so we
/// preserve that (a null/empty map value is dropped).
fn lower_environment(env: Environment) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match env {
        Environment::List(items) => {
            for item in items {
                if let Some((k, v)) = item.split_once('=') {
                    out.push((k.to_string(), v.to_string()));
                }
            }
        }
        Environment::Map(map) => {
            for (k, v) in map {
                let val = v.map(|s| s.into_string()).unwrap_or_default();
                if !val.is_empty() {
                    out.push((k, val));
                }
            }
        }
    }
    out
}

/// `ports`: the full compose grammar (CMP-P0-PORTS-FULL). The string/long-map
/// parsing + range expansion + proto/host_ip resolution lives in `ports.rs`;
/// here we lower each [`ParsedPort`] down to the `(host, container)` pair the
/// runtime `Service`/`Compose` type carries today.
///
/// The runtime `Service.ports` is `Vec<(u16, u16)>` (TCP-only, no proto/host_ip
/// — that model lives in `model.rs`, not owned by this WP). So at this boundary
/// we drop proto + host_ip, and — preserving the legacy parser, which IGNORED
/// short entries without a `:` (i.e. container-only) — we SKIP auto-assign
/// (`published == None`) entries. The full proto/host_ip-carrying `ParsedPort`
/// stays available for the WP that widens the runtime model.
///
/// Behavior-preserving: a plain `"H:C"` file still lowers to exactly `(H, C)`.
fn lower_ports(ports: &[super::spec::PortSpec]) -> Result<Vec<(u16, u16)>> {
    let parsed = parse_ports(ports)?;
    Ok(parsed
        .into_iter()
        .filter_map(|p: ParsedPort| p.published.map(|h| (h, p.target)))
        .collect())
}

/// Legacy `name=ref` list lowering for secrets/configs.
fn lower_pairs(items: &[String]) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|item| {
            let item = item.trim().trim_matches('"');
            item.split_once('=')
                .map(|(n, r)| (n.trim().to_string(), r.trim().to_string()))
        })
        .collect()
}

/// `healthcheck`: defaults interval=30s, retries=3 (legacy). The `test`/`cmd`
/// command is required — a healthcheck with no command is DROPPED (returns
/// `None`), matching the legacy parser.
fn lower_healthcheck(hc: Option<Healthcheck>) -> Result<Option<(String, u64, u32)>> {
    let Some(hc) = hc else {
        return Ok(None);
    };
    let cmd = match hc.test.or(hc.cmd) {
        Some(t) => lower_test(t),
        None => String::new(),
    };
    if cmd.is_empty() {
        return Ok(None);
    }
    let interval = match hc.interval {
        Some(v) => {
            let s = value_to_str(&v);
            parse_duration_secs(&s).ok_or_else(|| {
                LightrError::InvalidManifest(format!("bad healthcheck interval: {s}"))
            })?
        }
        None => 30,
    };
    let retries = hc.retries.unwrap_or(3);
    Ok(Some((cmd, interval, retries)))
}

/// `healthcheck.test`: a list strips a leading `CMD`/`CMD-SHELL` and joins the
/// rest with a space; a string is taken verbatim (quote-trimmed).
fn lower_test(t: StringOrList) -> String {
    match t {
        StringOrList::String(s) => s.trim().trim_matches('"').to_string(),
        StringOrList::List(mut parts) => {
            if parts
                .first()
                .map(|p| p == "CMD" || p == "CMD-SHELL")
                .unwrap_or(false)
            {
                parts.remove(0);
            }
            parts.join(" ")
        }
    }
}

/// Render a scalar YAML value as the string the duration parser expects
/// (`30`, `15s`, ...). Non-scalar values become an empty string (rejected).
fn value_to_str(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
#[path = "lower_tests.rs"]
mod tests;
