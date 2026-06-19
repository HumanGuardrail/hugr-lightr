//! Docker-compose YAML subset parser.
use lightr_core::{LightrError, Result};

use super::model::{empty_service, parse_duration_secs, Compose, Service};

/// Parse a minimal docker-compose YAML subset.
///
/// Supported structure (indentation-based, 2-space):
/// ```yaml
/// services:
///   <name>:
///     image: <ref>
///     command: "string" | ["a","b"]
///     ports:
///       - "H:C"
///     environment:
///       - K=V    # list form
///       K: V     # map form
///     x-lightr-eager: true
/// ```
/// Unknown keys are silently ignored. Returns `InvalidManifest` with the
/// 1-based line number on any structural parse error.
pub fn parse_compose(yaml: &str) -> Result<Compose> {
    enum ParseState {
        Top,
        Services,
        Service(String),
        Ports(String),
        Environment(String),
        Secrets(String),
        Configs(String),
        Healthcheck(String),
    }

    let mut state = ParseState::Top;
    let mut services: std::collections::HashMap<String, Service> = std::collections::HashMap::new();
    let mut service_order: Vec<String> = Vec::new();

    for (lineno0, raw_line) in yaml.lines().enumerate() {
        let lineno = lineno0 + 1;
        let stripped = raw_line.trim_end();
        if stripped.is_empty() || stripped.trim_start().starts_with('#') {
            continue;
        }
        let indent = stripped.len() - stripped.trim_start().len();
        let content = stripped.trim_start();

        match &state {
            ParseState::Top => {
                if content == "services:" {
                    state = ParseState::Services;
                }
            }
            ParseState::Services => {
                if indent == 2 && content.ends_with(':') {
                    let svc_name = content.trim_end_matches(':').to_string();
                    services.insert(svc_name.clone(), empty_service(svc_name.clone()));
                    service_order.push(svc_name.clone());
                    state = ParseState::Service(svc_name);
                }
            }
            ParseState::Service(svc) => {
                let svc = svc.clone();
                if indent == 2 && content.ends_with(':') {
                    let new_svc = content.trim_end_matches(':').to_string();
                    services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                    service_order.push(new_svc.clone());
                    state = ParseState::Service(new_svc);
                    continue;
                }
                if indent == 0 {
                    state = ParseState::Top;
                    continue;
                }
                if indent < 4 {
                    continue;
                }
                if content == "ports:" {
                    state = ParseState::Ports(svc);
                } else if content == "environment:" {
                    state = ParseState::Environment(svc);
                } else if content == "secrets:" {
                    state = ParseState::Secrets(svc);
                } else if content == "configs:" {
                    state = ParseState::Configs(svc);
                } else if content == "healthcheck:" {
                    state = ParseState::Healthcheck(svc);
                } else if let Some(val) = content.strip_prefix("image:") {
                    if let Some(s) = services.get_mut(&svc) {
                        s.image_ref = val.trim().to_string();
                    }
                } else if let Some(val) = content.strip_prefix("command:") {
                    let raw = val.trim();
                    let argv = if raw.starts_with('[') {
                        serde_json::from_str::<Vec<String>>(raw).map_err(|e| {
                            LightrError::InvalidManifest(format!(
                                "line {lineno}: bad command array: {e}"
                            ))
                        })?
                    } else {
                        vec!["/bin/sh".to_string(), "-c".to_string(), raw.to_string()]
                    };
                    if let Some(s) = services.get_mut(&svc) {
                        s.command = Some(argv);
                    }
                } else if let Some(val) = content.strip_prefix("x-lightr-eager:") {
                    if val.trim() == "true" {
                        if let Some(s) = services.get_mut(&svc) {
                            s.eager = true;
                        }
                    }
                }
            }
            ParseState::Ports(svc) => {
                let svc = svc.clone();
                let is_subkey = indent == 4 && content.ends_with(':') && !content.starts_with('-');
                if indent < 4 || is_subkey {
                    state = ParseState::Service(svc.clone());
                    if indent == 2 && content.ends_with(':') {
                        let new_svc = content.trim_end_matches(':').to_string();
                        services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                        service_order.push(new_svc.clone());
                        state = ParseState::Service(new_svc);
                    } else if is_subkey && content == "environment:" {
                        state = ParseState::Environment(svc);
                    }
                    continue;
                }
                let item = content.trim_start_matches("- ").trim().trim_matches('"');
                if let Some((h, c)) = item.split_once(':') {
                    let host: u16 = h.trim().parse().map_err(|_| {
                        LightrError::InvalidManifest(format!("line {lineno}: bad port: {item}"))
                    })?;
                    let cont: u16 = c.trim().parse().map_err(|_| {
                        LightrError::InvalidManifest(format!("line {lineno}: bad port: {item}"))
                    })?;
                    if let Some(s) = services.get_mut(&svc) {
                        s.ports.push((host, cont));
                    }
                }
            }
            ParseState::Environment(svc) => {
                let svc = svc.clone();
                let is_subkey = indent == 4 && content.ends_with(':') && !content.starts_with('-');
                if indent < 4 || is_subkey {
                    state = ParseState::Service(svc.clone());
                    if indent == 2 && content.ends_with(':') {
                        let new_svc = content.trim_end_matches(':').to_string();
                        services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                        service_order.push(new_svc.clone());
                        state = ParseState::Service(new_svc);
                    } else if is_subkey && content == "ports:" {
                        state = ParseState::Ports(svc);
                    }
                    continue;
                }
                let is_service_key = !content.starts_with('-');
                if is_service_key {
                    if content == "ports:" {
                        state = ParseState::Ports(svc);
                    } else if let Some(val) = content.strip_prefix("image:") {
                        if let Some(s) = services.get_mut(&svc) {
                            s.image_ref = val.trim().to_string();
                        }
                        state = ParseState::Service(svc);
                    } else if let Some(val) = content.strip_prefix("command:") {
                        let raw = val.trim();
                        let argv = if raw.starts_with('[') {
                            serde_json::from_str::<Vec<String>>(raw).map_err(|e| {
                                LightrError::InvalidManifest(format!(
                                    "line {lineno}: bad command array: {e}"
                                ))
                            })?
                        } else {
                            vec!["/bin/sh".to_string(), "-c".to_string(), raw.to_string()]
                        };
                        if let Some(s) = services.get_mut(&svc) {
                            s.command = Some(argv);
                        }
                        state = ParseState::Service(svc);
                    } else if let Some(val) = content.strip_prefix("x-lightr-eager:") {
                        if val.trim() == "true" {
                            if let Some(s) = services.get_mut(&svc) {
                                s.eager = true;
                            }
                        }
                        state = ParseState::Service(svc);
                    }
                    continue;
                }
                let item = if content.starts_with("- ") {
                    content.trim_start_matches("- ").trim()
                } else {
                    content
                };
                if let Some((k, v)) = item.split_once('=') {
                    if let Some(s) = services.get_mut(&svc) {
                        s.env.push((k.to_string(), v.to_string()));
                    }
                } else if let Some((k, v)) = item.split_once(':') {
                    let vt = v.trim();
                    if !vt.is_empty() {
                        if let Some(s) = services.get_mut(&svc) {
                            s.env.push((k.trim().to_string(), vt.to_string()));
                        }
                    }
                }
            }
            ParseState::Secrets(svc) | ParseState::Configs(svc) => {
                let svc = svc.clone();
                let is_secrets = matches!(state, ParseState::Secrets(_));
                let is_subkey = indent == 4 && content.ends_with(':') && !content.starts_with('-');
                if indent < 4 || is_subkey {
                    state = ParseState::Service(svc.clone());
                    if indent == 2 && content.ends_with(':') {
                        let new_svc = content.trim_end_matches(':').to_string();
                        services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                        service_order.push(new_svc.clone());
                        state = ParseState::Service(new_svc);
                    } else if is_subkey {
                        match content {
                            "ports:" => state = ParseState::Ports(svc),
                            "environment:" => state = ParseState::Environment(svc),
                            "secrets:" => state = ParseState::Secrets(svc),
                            "configs:" => state = ParseState::Configs(svc),
                            "healthcheck:" => state = ParseState::Healthcheck(svc),
                            _ => {}
                        }
                    }
                    continue;
                }
                let item = content.trim_start_matches("- ").trim().trim_matches('"');
                if let Some((name, refn)) = item.split_once('=') {
                    let pair = (name.trim().to_string(), refn.trim().to_string());
                    if let Some(s) = services.get_mut(&svc) {
                        if is_secrets {
                            s.secrets.push(pair);
                        } else {
                            s.configs.push(pair);
                        }
                    }
                }
            }
            ParseState::Healthcheck(svc) => {
                let svc = svc.clone();
                if indent <= 4 {
                    state = ParseState::Service(svc.clone());
                    if indent == 2 && content.ends_with(':') {
                        let new_svc = content.trim_end_matches(':').to_string();
                        services.insert(new_svc.clone(), empty_service(new_svc.clone()));
                        service_order.push(new_svc.clone());
                        state = ParseState::Service(new_svc);
                    } else if indent == 4 && content.ends_with(':') {
                        match content {
                            "ports:" => state = ParseState::Ports(svc),
                            "environment:" => state = ParseState::Environment(svc),
                            "secrets:" => state = ParseState::Secrets(svc),
                            "configs:" => state = ParseState::Configs(svc),
                            "healthcheck:" => state = ParseState::Healthcheck(svc),
                            _ => {}
                        }
                    }
                    continue;
                }
                if let Some(s) = services.get_mut(&svc) {
                    let hc = s.healthcheck.get_or_insert((String::new(), 30, 3));
                    if let Some(val) = content
                        .strip_prefix("test:")
                        .or_else(|| content.strip_prefix("cmd:"))
                    {
                        let raw = val.trim();
                        let cmd = if raw.starts_with('[') {
                            match serde_json::from_str::<Vec<String>>(raw) {
                                Ok(mut parts) => {
                                    if parts
                                        .first()
                                        .map(|p| p == "CMD" || p == "CMD-SHELL")
                                        .unwrap_or(false)
                                    {
                                        parts.remove(0);
                                    }
                                    parts.join(" ")
                                }
                                Err(e) => {
                                    return Err(LightrError::InvalidManifest(format!(
                                        "line {lineno}: bad healthcheck test array: {e}"
                                    )))
                                }
                            }
                        } else {
                            raw.trim_matches('"').to_string()
                        };
                        hc.0 = cmd;
                    } else if let Some(val) = content.strip_prefix("interval:") {
                        hc.1 = parse_duration_secs(val.trim()).ok_or_else(|| {
                            LightrError::InvalidManifest(format!(
                                "line {lineno}: bad healthcheck interval: {}",
                                val.trim()
                            ))
                        })?;
                    } else if let Some(val) = content.strip_prefix("retries:") {
                        hc.2 = val.trim().parse().map_err(|_| {
                            LightrError::InvalidManifest(format!(
                                "line {lineno}: bad healthcheck retries: {}",
                                val.trim()
                            ))
                        })?;
                    }
                }
            }
        }
    }

    for s in services.values_mut() {
        if let Some((cmd, _, _)) = &s.healthcheck {
            if cmd.is_empty() {
                s.healthcheck = None;
            }
        }
    }

    let ordered: Vec<Service> = service_order
        .into_iter()
        .filter_map(|n| services.remove(&n))
        .collect();

    Ok(Compose { services: ordered })
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
