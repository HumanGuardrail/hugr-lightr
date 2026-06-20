//! `lightr run` flag parsing + value types (skeleton-split from `mod.rs`).
//!
//! Behavior-preserving extraction of the cohesive parsing preamble: the JSON
//! report struct, the `--mount`/`--secret`/`--config`/`-p` value parsers, and
//! the `--health-*` flag bundle. No logic change — `run()` (in `mod.rs`) calls
//! these exactly as before; tests reach them via `super::` re-exports.

use lightr_core::validate_ref_name;
use lightr_run::healthcheck::Healthcheck;
use lightr_run::{Mount, PortMap, StoreFile};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct RunJson {
    pub(crate) key: String,
    pub(crate) hit: bool,
    pub(crate) exit_code: i32,
}

/// Parse a raw "ref:target" mount string into (ref_name, target).
/// Returns Err(exit_code) on validation failure (already printed to stderr).
pub(crate) fn parse_mount(raw: &str) -> Result<Mount, i32> {
    // Split on FIRST ':' only
    let colon = raw.find(':').ok_or_else(|| {
        eprintln!("lightr: invalid --mount value (missing ':'): {raw}");
        2i32
    })?;
    let ref_name = &raw[..colon];
    let target = &raw[colon + 1..];

    // Validate ref name
    if let Err(e) = validate_ref_name(ref_name) {
        eprintln!("lightr: invalid mount ref name: {e}");
        return Err(2);
    }

    // Validate target is relative (not absolute)
    if target.starts_with('/') {
        eprintln!("lightr: mount target must be relative, got: {target}");
        return Err(2);
    }

    Ok(Mount {
        ref_name: ref_name.to_string(),
        target: target.to_string(),
    })
}

/// Parse a raw "NAME=REF" secret/config string into a `StoreFile`.
/// Returns Err(exit_code) on a missing '=' (already printed to stderr).
pub(crate) fn parse_store_file(raw: &str, kind: &str) -> Result<StoreFile, i32> {
    let eq = raw.find('=').ok_or_else(|| {
        eprintln!("lightr: invalid --{kind} value (missing '='): {raw}");
        2i32
    })?;
    let name = &raw[..eq];
    let ref_name = &raw[eq + 1..];
    if name.is_empty() || ref_name.is_empty() {
        eprintln!("lightr: invalid --{kind} value (expected NAME=REF): {raw}");
        return Err(2);
    }
    Ok(StoreFile {
        name: name.to_string(),
        ref_name: ref_name.to_string(),
    })
}

/// Parse a raw `-p/--publish` value into a `PortMap` (Networking Phase 1).
///
/// Accepts `HOST:CONTAINER` or `HOST:CONTAINER/tcp`. Both ports must parse as
/// u16 in `1..=65535`. `…/udp` is rejected (UDP publish is Phase 2). On any bad
/// input prints to stderr and returns `Err(2)` (mirrors `parse_mount`).
pub(crate) fn parse_publish(raw: &str) -> Result<PortMap, i32> {
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

    let colon = body.find(':').ok_or_else(|| {
        eprintln!("lightr: invalid -p/--publish value (expected HOST:CONTAINER): {raw}");
        2i32
    })?;
    let host_str = &body[..colon];
    let container_str = &body[colon + 1..];

    let parse_port = |s: &str, which: &str| -> Result<u16, i32> {
        match s.parse::<u16>() {
            Ok(p) if (1..=65535).contains(&p) => Ok(p),
            _ => {
                eprintln!("lightr: invalid {which} port '{s}' in {raw} (expected 1..=65535)");
                Err(2)
            }
        }
    };

    let host = parse_port(host_str, "host")?;
    let container = parse_port(container_str, "container")?;
    Ok(PortMap { host, container })
}

/// The `--health-*` CLI flags, bundled (WP-RC-4). Built from the parsed `Cmd`
/// in dispatch and lowered to a [`Healthcheck`] by [`HealthFlags::build`].
///
/// `cmd == None` (no `--health-cmd`) OR `no_healthcheck == true` ⇒ no
/// healthcheck (the latter is Docker's `--no-healthcheck`, which wins over any
/// other `--health-*` flag). Otherwise the flags lower 1:1 to a [`Healthcheck`].
#[derive(Clone, Debug, Default)]
pub struct HealthFlags {
    pub cmd: Option<String>,
    pub interval: u64,
    pub timeout: u64,
    pub start_period: u64,
    pub retries: u32,
    pub no_healthcheck: bool,
}

impl HealthFlags {
    /// Lower the flags to a [`Healthcheck`], or `None` when no healthcheck is
    /// configured. `--no-healthcheck` disables unconditionally (Docker
    /// semantics); a missing `--health-cmd` is also "no healthcheck".
    pub fn build(&self) -> Option<Healthcheck> {
        if self.no_healthcheck {
            return None;
        }
        let cmd = self.cmd.clone()?;
        Some(Healthcheck {
            cmd,
            interval_s: self.interval,
            timeout_s: self.timeout,
            start_period_s: self.start_period,
            retries: self.retries,
        })
    }
}
