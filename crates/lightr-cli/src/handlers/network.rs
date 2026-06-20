//! `lightr network` handlers — container-network management (docker network).
//!
//! Docker-faithful + daemonless, over the C-wave net registry
//! (`lightr_run::NetworkRegistry`, persisted under `$LIGHTR_HOME/net/<id>/`).
//!
//! Verbs:
//!   network ls                       list predefined + user networks
//!   network create <name> [-d drv]   create a user network (error if exists)
//!   network rm <name>…               remove a user network (error if absent/
//!                                     predefined/in-use)
//!   network inspect <name> [--json]  print subnet + members
//!   network connect    <net> <ctr>   honest exit-2 (no daemonless hot-plug)
//!   network disconnect <net> <ctr>   honest exit-2 (no daemonless hot-plug)
//!
//! ## Why connect/disconnect are exit-2, not a no-op
//! Docker hot-plugs a running container's networks because a daemon owns the
//! veth/bridge state live. Lightr has no daemon: a container's networks are
//! fixed at spawn via `--network`. Silently succeeding would lie; we fail
//! closed with a usage-class (exit-2) error pointing at the real knob.
//!
//! ## Why we re-check existence around `create`
//! The registry's `create` is intentionally idempotent (open-if-present) for
//! the supervisor join path. Docker's `network create` errors if the name is
//! taken, so this handler enforces that contract before delegating.
//!
//! Tests inject a private tempdir `home` (house convention — see
//! `network::registry` / `run::registry`), never mutating the global env, so
//! they are parallel-safe under `cargo test --workspace`.

use std::io;
use std::path::{Path, PathBuf};

use lightr_core::LightrError;
use lightr_run::name_validate;
use lightr_run::network::{MacAddr, Member, NetworkRegistry, Subnet};
use serde::Serialize;

use crate::cli::cmd::NetworkCmd;
use crate::exit::{die_internal, die_lightr};
use crate::lightr_home;

/// The predefined networks Docker always presents (and which a user can never
/// create or remove): the default `bridge`, the host namespace `host`, and the
/// no-network `none`. Their driver mirrors Docker's `network ls` shape.
const PREDEFINED: &[(&str, &str)] = &[("bridge", "bridge"), ("host", "host"), ("none", "null")];

fn is_predefined(name: &str) -> bool {
    PREDEFINED.iter().any(|(n, _)| *n == name)
}

/// JSON-serialization failures are an internal invariant break, not user error;
/// surface them as `Io` (exit-1) rather than collapsing to a usage error.
fn json_err(e: serde_json::Error) -> LightrError {
    LightrError::Io(io::Error::other(e.to_string()))
}

/// `<home>/net/<name>` — the on-disk dir the registry owns for a network.
fn net_dir(home: &Path, name: &str) -> PathBuf {
    home.join("net").join(name)
}

// ── ls ──────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct NetRow {
    name: String,
    driver: String,
    scope: String,
}

/// Predefined rows + the user networks registered under `home`, in a stable
/// order (predefined first, then user networks sorted by the registry).
fn ls_rows(home: &Path) -> Result<Vec<NetRow>, LightrError> {
    let mut rows: Vec<NetRow> = PREDEFINED
        .iter()
        .map(|(name, driver)| NetRow {
            name: (*name).to_string(),
            driver: (*driver).to_string(),
            scope: "local".to_string(),
        })
        .collect();
    for id in NetworkRegistry::list(home).map_err(LightrError::Io)? {
        rows.push(NetRow {
            name: id,
            driver: "bridge".to_string(),
            scope: "local".to_string(),
        });
    }
    Ok(rows)
}

fn ls(home: &Path, json: bool) -> Result<(), LightrError> {
    let rows = ls_rows(home)?;
    if json {
        for row in &rows {
            println!("{}", serde_json::to_string(row).map_err(json_err)?);
        }
    } else {
        println!("{:<20}{:<12}SCOPE", "NAME", "DRIVER");
        for row in &rows {
            println!("{:<20}{:<12}{}", row.name, row.driver, row.scope);
        }
    }
    Ok(())
}

// ── create ────────────────────────────────────────────────────────────────────

fn create(home: &Path, name: &str) -> Result<(), LightrError> {
    name_validate(name)?;
    if is_predefined(name) {
        return Err(LightrError::InvalidRef(format!(
            "network '{name}' is a predefined network and cannot be created"
        )));
    }
    // Docker errors if the name is taken; the registry's create is idempotent,
    // so we reject an existing user network explicitly before delegating.
    if NetworkRegistry::open(home, &name.to_string()).is_ok() {
        return Err(LightrError::InvalidRef(format!(
            "network with name {name} already exists"
        )));
    }
    NetworkRegistry::create(home, &name.to_string()).map_err(LightrError::Io)?;
    println!("{name}");
    Ok(())
}

// ── rm ────────────────────────────────────────────────────────────────────────

fn rm_one(home: &Path, name: &str) -> Result<(), LightrError> {
    if is_predefined(name) {
        return Err(LightrError::InvalidRef(format!(
            "network '{name}' is a predefined network and cannot be removed"
        )));
    }
    let reg = NetworkRegistry::open(home, &name.to_string())
        .map_err(|_| LightrError::RefNotFound(format!("network {name}")))?;
    let members = reg.members().map_err(LightrError::Io)?;
    if !members.is_empty() {
        return Err(LightrError::Io(io::Error::other(format!(
            "network {name} has active endpoints ({} member(s)); cannot remove",
            members.len()
        ))));
    }
    std::fs::remove_dir_all(net_dir(home, name)).map_err(LightrError::Io)?;
    println!("{name}");
    Ok(())
}

fn rm(home: &Path, targets: &[String]) -> Result<(), LightrError> {
    if targets.is_empty() {
        return Err(LightrError::InvalidRef(
            "network rm requires at least one network".to_string(),
        ));
    }
    for name in targets {
        rm_one(home, name)?;
    }
    Ok(())
}

// ── inspect ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct MemberJson {
    name: String,
    mac: String,
    ipv4: String,
    aliases: Vec<String>,
}

#[derive(Serialize)]
struct InspectJson {
    name: String,
    subnet: String,
    gateway: String,
    members: Vec<MemberJson>,
}

fn mac_hex(m: &MacAddr) -> String {
    let b = m.0;
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5]
    )
}

fn member_json(m: &Member) -> MemberJson {
    MemberJson {
        name: m.name.clone(),
        mac: mac_hex(&m.mac),
        ipv4: m.ip.to_string(),
        aliases: m.aliases.clone(),
    }
}

fn subnet_cidr(s: &Subnet) -> String {
    format!("{}/{}", s.base, s.prefix)
}

fn inspect(home: &Path, target: &str) -> Result<(), LightrError> {
    if is_predefined(target) {
        return Err(LightrError::InvalidRef(format!(
            "network '{target}' is a predefined network with no registry record"
        )));
    }
    let reg = NetworkRegistry::open(home, &target.to_string())
        .map_err(|_| LightrError::RefNotFound(format!("network {target}")))?;
    let subnet = reg.subnet();
    let members = reg.members().map_err(LightrError::Io)?;
    let out = InspectJson {
        name: target.to_string(),
        subnet: subnet_cidr(&subnet),
        gateway: subnet.gateway.to_string(),
        members: members.iter().map(member_json).collect(),
    };
    println!("{}", serde_json::to_string_pretty(&out).map_err(json_err)?);
    Ok(())
}

// ── dispatch ────────────────────────────────────────────────────────────────

pub fn run(subcmd: NetworkCmd) -> i32 {
    let home = lightr_home();
    let result = match subcmd {
        NetworkCmd::Create { name, driver: _ } => create(&home, &name),
        NetworkCmd::Ls { json } => ls(&home, json),
        NetworkCmd::Rm { targets } => rm(&home, &targets),
        NetworkCmd::Inspect { target, json: _ } => inspect(&home, &target),
        // Daemonless model: no live hot-plug. Honest usage-class (exit-2) error.
        NetworkCmd::Connect { .. } | NetworkCmd::Disconnect { .. } => {
            return die_internal(
                &"live network connect/disconnect not supported; set --network at run",
            );
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => die_lightr(&e),
    }
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
