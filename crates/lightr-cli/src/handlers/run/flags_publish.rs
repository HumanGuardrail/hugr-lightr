//! `-p/--publish` + `-P/--publish-all` parsing (WP-B), split out of `flags.rs`
//! to keep that file under the 400-LOC godfile cap. Produces the EXISTING
//! `lightr_run::PortMap` spec type (host:container) — no type redesign.
//!
//! These functions are `pub(crate)` and re-exported from `flags` so the run
//! path's `super::parse_publish` call sites resolve unchanged.

use lightr_run::PortMap;

/// Validate a single port token as a u16 in `1..=65535`. Shared by the single
/// and range parsers so the error grammar is identical.
fn parse_port_token(s: &str, which: &str, raw: &str) -> Result<u16, i32> {
    match s.parse::<u16>() {
        Ok(p) if (1..=65535).contains(&p) => Ok(p),
        _ => {
            eprintln!("lightr: invalid {which} port '{s}' in {raw} (expected 1..=65535)");
            Err(2)
        }
    }
}

/// Parse a `-p` port token that is EITHER a single port (`8080`) or an inclusive
/// range (`8000-9000`) into the explicit list of ports it denotes. A single port
/// yields a 1-element list; a range yields `lo..=hi`. `lo > hi` is rejected
/// (honest error — no silent empty range).
fn parse_port_range(s: &str, which: &str, raw: &str) -> Result<Vec<u16>, i32> {
    match s.split_once('-') {
        None => Ok(vec![parse_port_token(s, which, raw)?]),
        Some((lo_s, hi_s)) => {
            let lo = parse_port_token(lo_s, which, raw)?;
            let hi = parse_port_token(hi_s, which, raw)?;
            if lo > hi {
                eprintln!(
                    "lightr: invalid {which} port range '{s}' in {raw} (low {lo} > high {hi})"
                );
                return Err(2);
            }
            Ok((lo..=hi).collect())
        }
    }
}

/// Strip an optional leading `HOST_IP:` from a `-p` body and validate it.
///
/// Accepts an IPv4 (`127.0.0.1`), a bracketed IPv6 (`[::1]`), or none (default
/// `0.0.0.0`). Returns `(host_ip, remainder)` where `remainder` is the
/// `HOST:CONTAINER` portion. Because `-p` grammar is `[HOST_IP:]HOST:CONTAINER`,
/// the host-ip is present only when the body has THREE colon-separated fields
/// (or a bracketed v6 prefix). Mirrors `parse_publish`'s fail-closed style.
///
/// RUNTIME BOUNDARY: the parsed `host_ip` is VALIDATED but not yet carried into
/// the runtime — `PortMap` (in `lightr-run`, not owned by this WP) has no
/// `host_ip` field and the port forwarder binds `127.0.0.1` unconditionally
/// (`lightr-run/src/portforward.rs`). Carrying the IP to the bind site requires
/// extending `PortMap`, a `lightr-run` change outside this WP's owned files.
fn split_host_ip(body: &str, raw: &str) -> Result<(std::net::IpAddr, String), i32> {
    use std::net::{IpAddr, Ipv4Addr};
    let default_ip = IpAddr::V4(Ipv4Addr::UNSPECIFIED); // 0.0.0.0
                                                        // Bracketed IPv6: `[::1]:HOST:CONTAINER`.
    if let Some(rest) = body.strip_prefix('[') {
        let close = rest.find(']').ok_or_else(|| {
            eprintln!("lightr: invalid -p/--publish host-ip (unclosed '[') in {raw}");
            2i32
        })?;
        let ip: IpAddr = rest[..close].parse().map_err(|_| {
            eprintln!(
                "lightr: invalid -p/--publish IPv6 host-ip '{}' in {raw}",
                &rest[..close]
            );
            2i32
        })?;
        let after = rest[close + 1..].strip_prefix(':').ok_or_else(|| {
            eprintln!("lightr: invalid -p/--publish value (expected ']:HOST:CONTAINER'): {raw}");
            2i32
        })?;
        return Ok((ip, after.to_string()));
    }
    // Three colon fields ⇒ IPv4 host-ip is the first; otherwise no host-ip.
    let parts: Vec<&str> = body.splitn(3, ':').collect();
    if parts.len() == 3 {
        let ip: IpAddr = parts[0].parse().map_err(|_| {
            eprintln!(
                "lightr: invalid -p/--publish host-ip '{}' in {raw}",
                parts[0]
            );
            2i32
        })?;
        return Ok((ip, format!("{}:{}", parts[1], parts[2])));
    }
    Ok((default_ip, body.to_string()))
}

/// Parse a raw `-p/--publish` value into the explicit list of `PortMap`s it
/// denotes (Networking Phase 1, WP-B).
///
/// Grammar: `[HOST_IP:]HOST:CONTAINER[/tcp|/udp]` where `HOST`/`CONTAINER` are a
/// single port or an inclusive range (`8000-9000`). Ranges expand element-wise;
/// host and container range widths MUST match (honest error otherwise). `…/udp`
/// is rejected (UDP publish is Phase 2). On any bad input prints to stderr and
/// returns `Err(2)` (mirrors `parse_mount`). The default host-ip is `0.0.0.0`;
/// see `split_host_ip` for the RUNTIME BOUNDARY on the parsed host-ip.
pub(crate) fn parse_publish_spec(raw: &str) -> Result<Vec<PortMap>, i32> {
    // Strip an optional `/proto` suffix. Only tcp is supported in v1.
    let (body, proto) = match raw.rsplit_once('/') {
        Some((b, p)) => (b, Some(p)),
        None => (raw, None),
    };
    match proto {
        None | Some("tcp") => {}
        Some("udp") => {
            eprintln!("lightr: invalid -p/--publish value ({raw}): udp publish is Phase 2");
            return Err(2);
        }
        Some(other) => {
            eprintln!("lightr: invalid -p/--publish protocol '{other}' in {raw} (expected tcp)");
            return Err(2);
        }
    }

    // Peel an optional `HOST_IP:` prefix (validated; runtime-carry is gated —
    // see split_host_ip), leaving the `HOST:CONTAINER` core.
    let (_host_ip, core) = split_host_ip(body, raw)?;

    let colon = core.find(':').ok_or_else(|| {
        eprintln!("lightr: invalid -p/--publish value (expected HOST:CONTAINER): {raw}");
        2i32
    })?;
    let host_str = &core[..colon];
    let container_str = &core[colon + 1..];

    let host_ports = parse_port_range(host_str, "host", raw)?;
    let container_ports = parse_port_range(container_str, "container", raw)?;
    if host_ports.len() != container_ports.len() {
        eprintln!(
            "lightr: invalid -p/--publish range widths in {raw} (host {} ports vs container {} ports)",
            host_ports.len(),
            container_ports.len()
        );
        return Err(2);
    }
    Ok(host_ports
        .into_iter()
        .zip(container_ports)
        .map(|(host, container)| PortMap { host, container })
        .collect())
}

/// Backward-compatible single-`PortMap` wrapper over [`parse_publish_spec`] for
/// callers expecting exactly one mapping per raw value; a multi-port range is
/// rejected here (range-aware callers use [`parse_publish_spec`] directly).
pub(crate) fn parse_publish(raw: &str) -> Result<PortMap, i32> {
    let mut maps = parse_publish_spec(raw)?;
    if maps.len() != 1 {
        eprintln!("lightr: -p/--publish range expands to {} mappings in {raw}; this caller expects a single port", maps.len());
        return Err(2);
    }
    Ok(maps.remove(0))
}

/// Synthesize publish mappings for Docker `-P/--publish-all`: one `PortMap` per
/// port the image EXPOSEs. `expose` is the OCI image config's exposed-port list
/// in Docker grammar (`"80/tcp"`, `"53/udp"`). UDP entries are skipped (Phase-1
/// is tcp-only). Malformed entries are skipped (an EXPOSE list is image-provided,
/// not user input — fail-soft rather than aborting the run).
///
/// RUNTIME BOUNDARY: Docker `-P` binds each port to a fresh EPHEMERAL host port.
/// `PortMap.host` is a concrete `1..=65535` (no "ephemeral" sentinel), so the
/// synthesized host == the exposed container port; the actual ephemeral-port
/// assignment is the forwarder's runtime concern (`lightr-run`, not owned here).
///
/// `allow(dead_code)`: this is the WP-B-provided EXPOSE→spec builder. Its lone
/// consumer is the `-P/--publish-all` branch in the run path (`mod.rs`/`paths.rs`,
/// where the image config that holds the EXPOSE list is loaded) — files OUTSIDE
/// this WP's owned set. Wiring it there is the (non-owned) caller-side follow-up;
/// the parse→spec contract here is frozen and unit-tested.
#[allow(dead_code)]
pub(crate) fn synth_publish_all(expose: &[String]) -> Vec<PortMap> {
    let mut out = Vec::new();
    for entry in expose {
        let (port_str, proto) = match entry.rsplit_once('/') {
            Some((p, pr)) => (p, Some(pr)),
            None => (entry.as_str(), None),
        };
        if matches!(proto, Some("udp")) {
            continue; // Phase-1 tcp-only.
        }
        if let Ok(port) = port_str.parse::<u16>() {
            if (1..=65535).contains(&port) {
                out.push(PortMap {
                    host: port,
                    container: port,
                });
            }
        }
    }
    out
}
