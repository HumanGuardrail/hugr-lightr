//! WP-CMP-NET: named-networks routing for the compose supervisor — the headline
//! "multi-service app talks by name".
//!
//! HYBRID model (LEAD decision): a service that DECLARES a `networks:`
//! attachment runs on the **vz** engine and joins the shared cross-process L2
//! switch (so peers resolve it BY SERVICE NAME via the embedded DNS); a service
//! on NO network stays NATIVE with today's loopback + env discovery, byte-
//! identical. This module owns ONLY the routing decision (engine + the
//! `RunSpec` network fields); the actual join + `switch_host::attach` is C9's
//! `svz.rs`, which fires automatically once `RunSpec.network` is `Some` (so this
//! module never re-implements the switch wiring — it only sets the trigger).
//!
//! Split out of `supervise.rs` for godfile headroom; consumed by
//! `start_one_instance`.

use lightr_core::{LightrError, Result};
use lightr_engine::EngineKind;
use lightr_run::PortMap;

use super::model::ServiceSpec;

/// The networking-derived inputs for a service's `RunSpec` + its engine choice.
///
/// A NATIVE service (no declared network) gets `engine = Native`, empty `ports`
/// (the supervisor's loopback proxy publishes on the native path, unchanged),
/// `run_name_for_dns = None`, and no network — byte-identical to today. A VZ
/// service (declared network) gets `engine = Vz`, its compose `ports` (the svz
/// forwarder + the registry port record publish them), `run_name_for_dns =
/// Some(service)` (so the switch seeds the DNS under the service name), and the
/// project-namespaced network id + aliases.
pub(crate) struct NetRouting {
    pub engine: EngineKind,
    pub ports: Vec<PortMap>,
    pub run_name_for_dns: Option<String>,
    pub network: Option<String>,
    pub network_alias: Vec<String>,
}

/// WP-CMP-NET: the registry network id for a service's FIRST declared network,
/// namespaced by project (`<project>_<network>`, Docker's per-project network
/// naming) + its aliases. `None` ⇒ the service declares no network ⇒ NATIVE.
///
/// A service on multiple networks attaches the FIRST here (one mesh NIC per vz
/// run today — the extras are surfaced, never silently dropped, by
/// [`note_unhonored_networks`]).
pub(crate) fn mesh_network_id(svc: &ServiceSpec, project: &str) -> Option<(String, Vec<String>)> {
    svc.networks
        .first()
        .map(|(name, aliases)| (format!("{project}_{name}"), aliases.clone()))
}

/// WP-CMP-NET: surface the additional networks a multi-network service does NOT
/// join (one mesh NIC per vz run today) so the gap is never a silent drop.
pub(crate) fn note_unhonored_networks(svc: &ServiceSpec, project: &str) {
    if svc.networks.len() > 1 {
        let extra: Vec<String> = svc.networks[1..]
            .iter()
            .map(|(n, _)| format!("{project}_{n}"))
            .collect();
        eprintln!(
            "lightr compose: service {:?}: attached to its FIRST network {:?}; \
             additional networks {extra:?} are RECORDED but not joined (one mesh \
             NIC per vz run today — never a silent drop).",
            svc.name,
            format!("{project}_{}", svc.networks[0].0),
        );
    }
}

/// WP-CMP-NET: the fail-closed routing decision for one service.
///
/// NO declared network ⇒ NATIVE (today's path, byte-identical). A declared
/// network ⇒ VZ + the switch: the service must boot a rootfs IMAGE (the switch
/// member is an `eth1` inside a microVM), so a networked service with NO usable
/// image is an HONEST error rather than a silent native fall-back that would
/// never resolve peers by name. Setting `network`/`run_name_for_dns` is the
/// trigger that makes C9's svz path join the registry + attach the switch.
pub(crate) fn route_networking(svc: &ServiceSpec, project: &str) -> Result<NetRouting> {
    let Some((network_id, aliases)) = mesh_network_id(svc, project) else {
        // Native path: no network, empty ports (loopback proxy owns publishing).
        return Ok(NetRouting {
            engine: EngineKind::Native,
            ports: Vec::new(),
            run_name_for_dns: None,
            network: None,
            network_alias: Vec::new(),
        });
    };

    if svc.image_ref.is_empty() || svc.image_ref == "scratch" {
        return Err(LightrError::InvalidManifest(format!(
            "compose service {:?} attaches network(s) {:?} but has no rootfs image: \
             a networked service must boot a rootfs on the vz engine to join the \
             switch (set `image:` to a Linux rootfs ref)",
            svc.name,
            svc.networks.iter().map(|(n, _)| n).collect::<Vec<_>>()
        )));
    }
    note_unhonored_networks(svc, project);

    // WP-B2: `PortMap` gained a `host_ip` field (Docker `-p HOST_IP:H:C`). A
    // compose service publishes on the default interface, so `PortMap::new`
    // (empty `host_ip` ⇒ `0.0.0.0`) is byte-identical to the prior two-field
    // literal — this is a mechanical construction-site update forced by the
    // shared-type field addition, not a behaviour change.
    let ports: Vec<PortMap> = svc
        .ports
        .iter()
        .map(|&(host, container)| PortMap::new(host, container))
        .collect();

    Ok(NetRouting {
        engine: EngineKind::Vz,
        ports,
        run_name_for_dns: Some(svc.name.clone()),
        network: Some(network_id),
        network_alias: aliases,
    })
}

/// Simple bidirectional byte proxy between two TCP streams. The native compose
/// path's lazy-start forwarder relays the first inbound connection to the just-
/// spawned service's loopback port (moved here from `supervise.rs` — the
/// networking plumbing lives with the routing — for godfile headroom).
pub(crate) fn proxy_bidirectional(a: std::net::TcpStream, b: std::net::TcpStream) {
    use std::io::{Read, Write};

    let a2 = a.try_clone();
    let b2 = b.try_clone();
    if a2.is_err() || b2.is_err() {
        return;
    }
    let mut a_read = a;
    let mut b_read = b;
    let mut a_write = a2.unwrap();
    let mut b_write = b2.unwrap();

    let t1 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match a_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if b_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let t2 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match b_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if a_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let _ = t1.join();
    let _ = t2.join();
}

#[cfg(test)]
#[path = "supervise_net_tests.rs"]
mod tests;
