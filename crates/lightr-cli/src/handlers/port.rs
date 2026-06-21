//! `lightr port` handler — list a container's published port mappings (docker port).
//!
//! Faithful to `docker port`: with no PORT arg, prints every published mapping
//! one per line in docker's format `<container>/<proto> -> 0.0.0.0:<host>`;
//! with a `[PORT]` arg (`8080` or `8080/tcp`), prints ONLY that container
//! port's host binding (`0.0.0.0:<host>`), or exits non-zero if that port is
//! not published. The mappings are read CLI-side from the run's `spec.json`.

use serde::Deserialize;

use crate::lightr_home;

// ── spec.json mirror (read-only, CLI-side) ──────────────────────────────────
//
// `SpecOnDisk` lives in `lightr-run` and is `pub(crate)` there — not reachable
// from this crate. We deserialize ONLY the published-port fields, each
// `#[serde(default)]` so a spec.json written before a field existed still
// parses (back-compat, mirroring SpecOnDisk's own serde defaults). House
// convention for CLI-side spec reads: `handlers::inspect::read_spec`.
#[derive(Deserialize, Default)]
struct PortSpec {
    /// Legacy TCP-only `(host, container)` channel.
    #[serde(default)]
    ports: Vec<(u16, u16)>,
    /// Go-forward proto-tagged channel.
    #[serde(default)]
    ports2: Vec<PortOnDisk>,
}

#[derive(Deserialize)]
struct PortOnDisk {
    host: u16,
    container: u16,
    #[serde(default = "default_proto")]
    proto: String,
}

fn default_proto() -> String {
    "tcp".to_string()
}

/// A single normalised published mapping: `(container, host, proto)`.
struct Mapping {
    container: u16,
    host: u16,
    proto: String,
}

/// Read + normalise the run's published mappings from `spec.json`. Absent /
/// unreadable / malformed ⇒ empty (fail-closed: we report no mappings rather
/// than fabricate one). `ports2` (proto-tagged) wins; legacy `ports` fills in
/// any container port `ports2` does not already cover (TCP), so a run written
/// either way reports correctly.
fn read_mappings(run_dir: &std::path::Path) -> Vec<Mapping> {
    let spec: PortSpec = std::fs::read(run_dir.join("spec.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();

    let mut out: Vec<Mapping> = spec
        .ports2
        .into_iter()
        .map(|p| Mapping {
            container: p.container,
            host: p.host,
            proto: p.proto,
        })
        .collect();

    for (host, container) in spec.ports {
        let covered = out
            .iter()
            .any(|m| m.container == container && m.proto == "tcp");
        if !covered {
            out.push(Mapping {
                container,
                host,
                proto: "tcp".to_string(),
            });
        }
    }
    out
}

/// Parse a `docker port` PORT arg: bare `8080` ⇒ `(8080, "tcp")`, or
/// `8080/udp` ⇒ `(8080, "udp")`. An unparseable port ⇒ `None`.
fn parse_port_arg(arg: &str) -> Option<(u16, String)> {
    let (num, proto) = match arg.split_once('/') {
        Some((n, p)) => (n, p.to_ascii_lowercase()),
        None => (arg, "tcp".to_string()),
    };
    num.trim().parse::<u16>().ok().map(|n| (n, proto))
}

pub fn run(target: &str, port: Option<&str>) -> i32 {
    let home = lightr_home();

    let id = match lightr_run::resolve(&home, target) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };

    let run_dir = home.join("run").join(&id);
    let mappings = read_mappings(&run_dir);

    match port {
        // Specific container port: print just `0.0.0.0:<host>` for it, or
        // fail-closed if it is not published (docker exits non-zero + errors).
        Some(p) => {
            let (want, proto) = match parse_port_arg(p) {
                Some(parsed) => parsed,
                None => {
                    eprintln!("Error: invalid port: {p}");
                    return 1;
                }
            };
            match mappings
                .iter()
                .find(|m| m.container == want && m.proto == proto)
            {
                Some(m) => {
                    println!("0.0.0.0:{}", m.host);
                    0
                }
                None => {
                    eprintln!("Error: No public port '{want}/{proto}' published for {target}");
                    1
                }
            }
        }
        // No PORT arg: print every mapping, docker format, one per line.
        None => {
            for m in &mappings {
                println!("{}/{} -> 0.0.0.0:{}", m.container, m.proto, m.host);
            }
            0
        }
    }
}

#[cfg(test)]
#[path = "port_tests.rs"]
mod tests;
