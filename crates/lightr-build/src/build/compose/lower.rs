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
use super::model::{empty_service, parse_duration_secs, Compose, LoweredHealthcheck, Service};
use super::ports::{parse_ports, ParsedPort};
use super::spec::{ComposeSpec, Environment, Healthcheck, ServiceDef, StringOrList};
// SKELETON-FREEZE: per-aspect stubs for the frozen-but-not-yet-lowered service
// fields live in their own module so a feature WP fills exactly ONE stub body
// without colliding on this file. The dispatcher (`lower_service`) calls each.
use super::lower_stubs;

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
    // WP-CMP-SECRETS-FULL: lower the top-level `secrets:`/`configs:` source maps
    // BEFORE the services map is consumed, so the up-path can ingest each `file:`
    // source into the Store (the lowering holds no Store; ingestion is up.rs's).
    let secret_sources = super::lower_files::lower_top_sources(&spec.secrets);
    let config_sources = super::lower_files::lower_top_sources(&spec.configs);
    let mut services = Vec::with_capacity(spec.services.len());
    for (name, def) in spec.services {
        services.push(lower_service(name, def, base_dir)?);
    }
    Ok(Compose {
        services,
        secret_sources,
        config_sources,
    })
}

/// Dispatcher: lower one service `def` into a runtime [`Service`] by calling each
/// per-aspect `lower_<aspect>` helper in turn.
///
/// SKELETON-FREEZE: this is intentionally a flat list of one call per compose
/// aspect. Aspects that ALREADY lower today (image/command/env/ports/eager/
/// secrets/configs/healthcheck) live in this file; aspects that are frozen in
/// the model but not yet lowered (depends_on/deploy/networks/restart/...) are
/// honest no-op stubs in [`super::lower_stubs`]. A feature WP fills exactly ONE
/// stub body and adds nothing here, so WPs stay disjoint — no collision on the
/// dispatcher beyond the (already-present) call site.
///
/// Behavior-preserving: the active aspects below run in the SAME order and
/// produce the byte-identical `Service` the legacy single-function path did; the
/// stub calls do nothing.
fn lower_service(name: String, mut def: ServiceDef, base_dir: Option<&Path>) -> Result<Service> {
    let mut svc = empty_service(name);

    // --- active aspects (already lowered; behavior-preserving) ---
    lower_image(&def, &mut svc);
    lower_command_aspect(&def, &mut svc);
    lower_env_aspect(&mut def, &mut svc, base_dir)?;
    lower_ports_aspect(&def, &mut svc)?;
    lower_eager(&def, &mut svc);
    lower_secrets(&def, &mut svc);
    lower_configs(&def, &mut svc);
    lower_healthcheck_aspect(&def, &mut svc)?;

    // --- frozen-but-not-yet-lowered aspects (SKELETON-FREEZE stubs) ---
    lower_stubs::lower_depends_on(&def, &mut svc);
    lower_stubs::lower_deploy(&def, &mut svc);
    lower_stubs::lower_networks(&def, &mut svc);
    lower_stubs::lower_restart(&def, &mut svc);
    // WP-CMP-SECRETS-FULL: the full-spec secrets/configs refs are lowered above
    // by lower_secrets/lower_configs (single writer); the disconnected
    // lower_spec_secrets/lower_spec_configs stubs are removed (they could not
    // reach the top-level source maps and would double-handle def.secrets).
    lower_stubs::lower_extra_hosts(&def, &mut svc);
    lower_stubs::lower_stop_grace_period(&def, &mut svc);
    lower_stubs::lower_stop_signal(&def, &mut svc);
    lower_stubs::lower_init(&def, &mut svc);
    lower_stubs::lower_tty(&def, &mut svc);
    lower_stubs::lower_cap_add(&def, &mut svc);
    lower_stubs::lower_cap_drop(&def, &mut svc);
    lower_stubs::lower_privileged(&def, &mut svc);
    lower_stubs::lower_container_name(&def, &mut svc);
    lower_stubs::lower_working_dir(&def, &mut svc);
    lower_stubs::lower_user(&def, &mut svc);
    lower_stubs::lower_entrypoint(&def, &mut svc);
    lower_stubs::lower_profiles(&def, &mut svc);

    Ok(svc)
}

/// `image`: the container image reference. Empty when the service declares only
/// a `build:` (not lowered yet).
fn lower_image(def: &ServiceDef, svc: &mut Service) {
    if let Some(image) = &def.image {
        svc.image_ref = image.clone();
    }
}

/// `command` aspect: lower the service's `command` (string ⇒ `/bin/sh -c`
/// wrapper, list ⇒ argv as-is) onto `svc.command`.
fn lower_command_aspect(def: &ServiceDef, svc: &mut Service) {
    svc.command = def.command.as_ref().map(lower_command);
}

/// `environment`/`env_file` aspect: fold env sources (env_file first, inline on
/// top) onto `svc.env`. See [`lower_env`] for precedence. Takes `&mut def` to
/// MOVE the `environment` block out (folding consumes the scalar map values via
/// `EnvScalar::into_string`); `env_file` is borrowed (only its paths are read).
fn lower_env_aspect(
    def: &mut ServiceDef,
    svc: &mut Service,
    base_dir: Option<&Path>,
) -> Result<()> {
    svc.env = lower_env(def.env_file.as_ref(), def.environment.take(), base_dir)?;
    Ok(())
}

/// `ports` aspect: lower the parsed compose ports onto `svc.ports`.
fn lower_ports_aspect(def: &ServiceDef, svc: &mut Service) -> Result<()> {
    svc.ports = lower_ports(&def.ports)?;
    Ok(())
}

/// `x-lightr-eager` aspect: the Lightr eager-start extension.
fn lower_eager(def: &ServiceDef, svc: &mut Service) {
    svc.eager = def.x_lightr_eager.unwrap_or(false);
}

/// `secrets` aspect (WP-CMP-SECRETS-FULL): a service's `secrets:` refs ⇒
/// `svc.secrets` `(name, ref)` pairs feeding `RunSpec.secrets`. The full
/// resolution (legacy `name=ref`, compose short name, compose long map) lives in
/// `lower_files.rs` (single writer; the top-level source ingestion is up.rs's).
fn lower_secrets(def: &ServiceDef, svc: &mut Service) {
    svc.secrets = super::lower_files::lower_service_file_refs(&def.secrets, "secret");
}

/// `configs` aspect (WP-CMP-SECRETS-FULL): counterpart of [`lower_secrets`].
fn lower_configs(def: &ServiceDef, svc: &mut Service) {
    svc.configs = super::lower_files::lower_service_file_refs(&def.configs, "config");
}

/// `healthcheck` aspect: lower the full compose healthcheck onto
/// `svc.healthcheck`. See [`lower_healthcheck`].
fn lower_healthcheck_aspect(def: &ServiceDef, svc: &mut Service) -> Result<()> {
    svc.healthcheck = lower_healthcheck(def.healthcheck.as_ref())?;
    Ok(())
}

/// `command`: a bare string becomes a `/bin/sh -c` wrapper (legacy semantics);
/// a list is taken as the argv as-is.
fn lower_command(c: &StringOrList) -> Vec<String> {
    match c {
        StringOrList::String(s) => {
            vec!["/bin/sh".to_string(), "-c".to_string(), s.clone()]
        }
        StringOrList::List(v) => v.clone(),
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

/// `healthcheck` (CMP-P1-HEALTH-FULL): lower the full compose form to the
/// runtime tuple `(cmd, interval_s, timeout_s, start_period_s, retries)`.
///
/// Docker-faithful defaults are applied per missing field: interval 30s,
/// timeout 30s, start_period 0s, retries 3 (matching the runtime
/// `Healthcheck::new` defaults). A healthcheck is DROPPED (returns `None`) when:
///  * `disable: true` (the explicit compose toggle), or
///  * `test`/`cmd` is `NONE` (`["NONE"]` or the string `"NONE"`), or
///  * no command is present at all (back-compat with the legacy parser).
fn lower_healthcheck(hc: Option<&Healthcheck>) -> Result<Option<LoweredHealthcheck>> {
    let Some(hc) = hc else {
        return Ok(None);
    };
    // `disable: true` ⇒ no healthcheck, regardless of any other field.
    if hc.disable == Some(true) {
        return Ok(None);
    }
    let cmd = match hc.test.as_ref().or(hc.cmd.as_ref()) {
        // `test: NONE` disables the healthcheck.
        Some(t) => match lower_test(t) {
            None => return Ok(None),
            Some(c) => c,
        },
        None => String::new(),
    };
    if cmd.is_empty() {
        return Ok(None);
    }
    let interval = duration_field(hc.interval.as_ref(), 30, "interval")?;
    let timeout = duration_field(hc.timeout.as_ref(), 30, "timeout")?;
    let start_period = duration_field(hc.start_period.as_ref(), 0, "start_period")?;
    let retries = hc.retries.unwrap_or(3);
    Ok(Some((cmd, interval, timeout, start_period, retries)))
}

/// Parse an optional compose duration field, falling back to `default` when
/// absent. A present-but-unparseable value is a fail-closed error.
fn duration_field(v: Option<&serde_yaml::Value>, default: u64, name: &str) -> Result<u64> {
    match v {
        Some(v) => {
            let s = value_to_str(v);
            parse_duration_secs(&s)
                .ok_or_else(|| LightrError::InvalidManifest(format!("bad healthcheck {name}: {s}")))
        }
        None => Ok(default),
    }
}

/// `healthcheck.test`: returns `None` when the test DISABLES the healthcheck
/// (`NONE` in either string or `["NONE"]` list form). Otherwise a list strips a
/// leading `CMD`/`CMD-SHELL` and joins the rest with a space; a string is taken
/// verbatim (quote-trimmed).
fn lower_test(t: &StringOrList) -> Option<String> {
    match t {
        StringOrList::String(s) => {
            let s = s.trim().trim_matches('"').to_string();
            if s == "NONE" {
                None
            } else {
                Some(s)
            }
        }
        StringOrList::List(parts) => {
            let rest: &[String] = match parts.first().map(String::as_str) {
                // `["NONE"]` disables the healthcheck.
                Some("NONE") => return None,
                // exec/shell forms strip the leading directive.
                Some("CMD") | Some("CMD-SHELL") => &parts[1..],
                _ => &parts[..],
            };
            Some(rest.join(" "))
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
