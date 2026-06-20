//! Full compose-spec `ports` grammar (CMP-P0-PORTS-FULL).
//!
//! Today `lower.rs` only understood the short `"HOST:CONTAINER"` string. This
//! module is the complete parser for the docker-compose `ports` short + long
//! syntax, lowered to a proto-tagged [`ParsedPort`]:
//!
//! Short string forms:
//!   - `"80"`                     container-only, host auto-assigned
//!   - `"8080:80"`                host:container
//!   - `"127.0.0.1:8080:80"`      host_ip:host:container
//!   - `"8080:80/udp"`            protocol suffix
//!   - `"3000-3005:3000-3005"`    ranges (expand to N mappings)
//!   - host_ip + range + proto compose, e.g. `"127.0.0.1:3000-3001:3000-3001/udp"`
//!
//! Long mapping form (one entry):
//!   `{ target: 80, published: 8080, protocol: tcp, host_ip: 127.0.0.1, mode: host }`
//!
//! Defaults (matching Lightr's loopback-publish model + the legacy short
//! parser): protocol `tcp`, host_ip `127.0.0.1`. Container-only short forms
//! auto-assign the host port (modeled as `published == None`).
//!
//! Fail-closed: any malformed spec, a non-string/non-int short scalar, or a
//! range whose host span length differs from its container span length is an
//! `InvalidManifest` error — never silently dropped.
use lightr_core::{LightrError, Result};

use super::spec::PortSpec;

/// The default published host_ip — Lightr publishes on the loopback interface
/// (see `run::types::PortMap` "on 127.0.0.1").
pub(crate) const DEFAULT_HOST_IP: &str = "127.0.0.1";
/// The default protocol — TCP, matching the legacy short parser and
/// `run::types::PortOnDisk::proto`'s serde default.
pub(crate) const DEFAULT_PROTO: &str = "tcp";

/// One lowered, fully-resolved published port mapping. Carries the proto +
/// host_ip the compose-spec allows; ranges have already been expanded so each
/// `ParsedPort` is a single host↔container pair.
///
/// `published == None` ⇒ the short container-only form (`"80"`): the host port
/// is auto-assigned by the runtime, exactly like Docker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedPort {
    /// Host bind address. Defaults to [`DEFAULT_HOST_IP`].
    pub host_ip: String,
    /// Published host port. `None` ⇒ auto-assign (container-only short form).
    pub published: Option<u16>,
    /// Container (target) port.
    pub target: u16,
    /// `"tcp"` (default) or `"udp"`.
    pub proto: String,
}

/// Parse a list of compose `ports` entries into the expanded, proto-tagged
/// mappings. Ranges expand in declaration order.
pub(crate) fn parse_ports(ports: &[PortSpec]) -> Result<Vec<ParsedPort>> {
    let mut out = Vec::new();
    for p in ports {
        match p {
            PortSpec::Short(v) => parse_short(v, &mut out)?,
            PortSpec::Long(m) => out.push(parse_long(m)?),
        }
    }
    Ok(out)
}

fn bad(spec: &str) -> LightrError {
    LightrError::InvalidManifest(format!("bad port: {spec}"))
}

/// Parse a short-syntax scalar (string or YAML number) and append its
/// (possibly range-expanded) mappings.
fn parse_short(v: &serde_yaml::Value, out: &mut Vec<ParsedPort>) -> Result<()> {
    let raw = match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) if n.is_u64() => n.to_string(),
        _ => return Err(bad("<non-scalar short port>")),
    };
    let spec = raw.trim().trim_matches('"').trim();
    if spec.is_empty() {
        return Err(bad(&raw));
    }

    // Split off a trailing `/proto`.
    let (body, proto) = match spec.split_once('/') {
        Some((b, p)) => {
            let p = p.trim().to_ascii_lowercase();
            if p.is_empty() {
                return Err(bad(spec));
            }
            (b.trim(), p)
        }
        None => (spec, DEFAULT_PROTO.to_string()),
    };

    // Colon-split the body. From the right: the last field is the container
    // (range), the middle (optional) is the host (range), and an optional
    // leading field(s) form the host_ip (which may itself contain `:` for
    // IPv6, so we rebuild it from the remaining left segments).
    let parts: Vec<&str> = body.split(':').collect();
    let (host_ip, host_field, cont_field) = match parts.as_slice() {
        [cont] => (DEFAULT_HOST_IP.to_string(), None, *cont),
        [host, cont] => (DEFAULT_HOST_IP.to_string(), Some(*host), *cont),
        // 3+ segments: container last, host second-to-last, the rest is host_ip
        // (IPv6 literals contain colons).
        [ip_parts @ .., host, cont] => (ip_parts.join(":"), Some(*host), *cont),
        [] => return Err(bad(spec)),
    };

    let cont_range = parse_range(cont_field, spec)?;
    match host_field {
        None => {
            // Container-only: host auto-assigned for each container port.
            for c in cont_range {
                out.push(ParsedPort {
                    host_ip: host_ip.clone(),
                    published: None,
                    target: c,
                    proto: proto.clone(),
                });
            }
        }
        Some(hf) if hf.trim().is_empty() => {
            // `host_ip::container` — host port auto-assigned, host_ip kept.
            for c in cont_range {
                out.push(ParsedPort {
                    host_ip: host_ip.clone(),
                    published: None,
                    target: c,
                    proto: proto.clone(),
                });
            }
        }
        Some(hf) => {
            let host_range = parse_range(hf, spec)?;
            if host_range.len() != cont_range.len() {
                return Err(LightrError::InvalidManifest(format!(
                    "bad port: {spec} (host range len {} != container range len {})",
                    host_range.len(),
                    cont_range.len()
                )));
            }
            for (h, c) in host_range.into_iter().zip(cont_range) {
                out.push(ParsedPort {
                    host_ip: host_ip.clone(),
                    published: Some(h),
                    target: c,
                    proto: proto.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Parse a single port field as either a scalar `"80"` or an inclusive range
/// `"3000-3005"`, returning every port in the (inclusive) span. Fail-closed on
/// non-numeric bounds or an inverted range.
fn parse_range(field: &str, spec: &str) -> Result<Vec<u16>> {
    let field = field.trim();
    if let Some((lo, hi)) = field.split_once('-') {
        let lo: u16 = lo.trim().parse().map_err(|_| bad(spec))?;
        let hi: u16 = hi.trim().parse().map_err(|_| bad(spec))?;
        if hi < lo {
            return Err(bad(spec));
        }
        Ok((lo..=hi).collect())
    } else {
        let p: u16 = field.parse().map_err(|_| bad(spec))?;
        Ok(vec![p])
    }
}

/// Parse the long mapping form. `target` is required; `published` is optional
/// (auto-assign when absent); `protocol`/`host_ip` default per the model.
fn parse_long(m: &super::spec::PortLong) -> Result<ParsedPort> {
    let target = m
        .target
        .ok_or_else(|| LightrError::InvalidManifest("bad port: long form missing target".into()))?;
    let proto = m
        .protocol
        .as_deref()
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| DEFAULT_PROTO.to_string());
    let host_ip = m
        .host_ip
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_HOST_IP)
        .to_string();
    Ok(ParsedPort {
        host_ip,
        published: m.published,
        target,
        proto,
    })
}

#[cfg(test)]
#[path = "ports_tests.rs"]
mod tests;
