//! compose_supervise + helpers: start_service_detached, proxy_bidirectional, discovery_key,
//! prepare_service_cwd.
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::{Path, PathBuf};

use super::model::{ServiceSpec, StackSpec};
use super::up::lightr_home_pub as lightr_home;

/// Prepare a clean per-service run directory and, if the service declares an
/// `image_ref`, hydrate that ref's filesystem into it.
pub(crate) fn prepare_service_cwd(svc: &ServiceSpec, store: &Store) -> Result<PathBuf> {
    let cwd = std::env::temp_dir().join(format!("lightr-svc-{}", svc.name));
    if cwd.exists() {
        std::fs::remove_dir_all(&cwd).map_err(LightrError::Io)?;
    }
    std::fs::create_dir_all(&cwd).map_err(LightrError::Io)?;
    if !svc.image_ref.is_empty() && svc.image_ref != "scratch" {
        lightr_index::hydrate(&cwd, store, &svc.image_ref)?;
    }
    Ok(cwd)
}

/// WP-DISC: sanitize a compose service name into an env-var key prefix.
pub(crate) fn discovery_key(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Spawn a service as a detached lightr run.
pub(crate) fn start_service_detached(
    stack_dir: &Path,
    svc: &ServiceSpec,
    peers: &[(String, u16)],
) -> Result<()> {
    use lightr_run::healthcheck::Healthcheck;
    use lightr_run::{spawn_detached_engine, Mount, RunSpec, StoreFile};

    let store_root = lightr_home().join("store");
    let store = Store::open(&store_root)?;
    let cwd = prepare_service_cwd(svc, &store)?;

    let to_store_files = |pairs: &[(String, String)]| -> Vec<StoreFile> {
        pairs
            .iter()
            .map(|(name, ref_name)| StoreFile {
                name: name.clone(),
                ref_name: ref_name.clone(),
            })
            .collect()
    };

    let spec = RunSpec {
        cwd: cwd.clone(),
        inputs: Vec::new(),
        command: svc.command.clone(),
        env_keys: svc.env.iter().map(|(k, _)| k.clone()).collect(),
        mounts: Vec::new() as Vec<Mount>,
        secrets: to_store_files(&svc.secrets),
        configs: to_store_files(&svc.configs),
        ports: Vec::new(),
    };

    let mut child_env: Vec<(String, String)> = svc.env.clone();
    for (peer_name, container_port) in peers {
        if peer_name == &svc.name {
            continue;
        }
        let prefix = discovery_key(peer_name);
        child_env.push((format!("{prefix}_HOST"), "127.0.0.1".to_string()));
        child_env.push((format!("{prefix}_PORT"), container_port.to_string()));
    }

    // CMP-P1-HEALTH-FULL: compose now lowers the full healthcheck — cmd,
    // interval, timeout, start_period, retries — straight into the runtime
    // `Healthcheck` (the RC-4 fields are no longer hardcoded defaults).
    let hc =
        svc.healthcheck
            .as_ref()
            .map(
                |(cmd, interval_s, timeout_s, start_period_s, retries)| Healthcheck {
                    cmd: cmd.clone(),
                    interval_s: *interval_s,
                    timeout_s: *timeout_s,
                    start_period_s: *start_period_s,
                    retries: *retries,
                },
            );

    let handle = spawn_detached_engine(
        &spec,
        &store,
        hc.as_ref(),
        lightr_engine::EngineKind::Native,
        None,
        &child_env,
    )?;

    let spec_path = stack_dir.join("spec.json");
    if let Ok(bytes) = std::fs::read(&spec_path) {
        if let Ok(mut stack_spec) = serde_json::from_slice::<StackSpec>(&bytes) {
            for s in &mut stack_spec.services {
                if s.name == svc.name {
                    s.run_dir = Some(handle.dir.to_string_lossy().into_owned());
                }
            }
            if let Ok(new_bytes) = serde_json::to_vec_pretty(&stack_spec) {
                let _ = std::fs::write(&spec_path, &new_bytes);
            }
        }
    }

    Ok(())
}

/// Simple bidirectional byte proxy between two TCP streams.
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

/// Compose supervisor -- called by `lightr __compose-supervise <stack_dir>`.
pub fn compose_supervise(stack_dir: &Path) -> Result<()> {
    use std::time::{Duration, Instant};

    let spec_path = stack_dir.join("spec.json");
    let spec_bytes = std::fs::read(&spec_path).map_err(LightrError::Io)?;
    let mut spec: StackSpec = serde_json::from_slice(&spec_bytes)
        .map_err(|e| LightrError::InvalidManifest(format!("stack spec parse: {e}")))?;

    let pid = std::process::id();
    std::fs::write(stack_dir.join("pid"), pid.to_string().as_bytes()).map_err(LightrError::Io)?;
    spec.supervisor_pid = Some(pid);
    let spec_bytes2 = serde_json::to_vec_pretty(&spec)
        .map_err(|e| LightrError::InvalidManifest(format!("serialize: {e}")))?;
    std::fs::write(&spec_path, &spec_bytes2).map_err(LightrError::Io)?;

    let ttl = Duration::from_secs(spec.ttl_secs);
    let start = Instant::now();
    let stop_file = stack_dir.join("stop");

    let peers: Vec<(String, u16)> = spec
        .services
        .iter()
        .filter_map(|s| {
            s.ports
                .first()
                .map(|&(_, container)| (s.name.clone(), container))
        })
        .collect();

    for svc in &spec.services {
        if svc.eager && !svc.command.is_empty() {
            start_service_detached(stack_dir, svc, &peers)?;
        }
    }

    let mut threads: Vec<std::thread::JoinHandle<()>> = Vec::new();

    for svc_spec in &spec.services {
        if svc_spec.eager {
            continue;
        }
        for &(host_port, container_port) in &svc_spec.ports {
            let addr = format!("127.0.0.1:{host_port}");
            let listener = match std::net::TcpListener::bind(&addr) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(
                        "lightr compose: bind {addr} for service {} failed: {e}",
                        svc_spec.name
                    );
                    continue;
                }
            };
            let svc_clone = svc_spec.clone();
            let stack_dir_clone = stack_dir.to_path_buf();
            let peers_clone = peers.clone();
            let jh = std::thread::spawn(move || {
                if let Ok((inbound, _)) = listener.accept() {
                    if let Err(e) =
                        start_service_detached(&stack_dir_clone, &svc_clone, &peers_clone)
                    {
                        eprintln!("lightr compose: failed to start {}: {e}", svc_clone.name);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let svc_addr = format!("127.0.0.1:{container_port}");
                    if let Ok(outbound) = std::net::TcpStream::connect(&svc_addr) {
                        proxy_bidirectional(inbound, outbound);
                    }
                }
            });
            threads.push(jh);
        }
    }

    loop {
        if stop_file.exists() || start.elapsed() >= ttl {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightr_store::Store;
    use tempfile::TempDir;

    #[test]
    fn prepare_service_cwd_hydrates_image_ref() {
        let src = TempDir::new().unwrap();
        std::fs::write(src.path().join("marker.txt"), b"from-image").unwrap();
        let store_tmp = TempDir::new().unwrap();
        let store = Store::open(store_tmp.path()).unwrap();
        lightr_index::snapshot(src.path(), &store, "svc-img").unwrap();
        let svc = ServiceSpec {
            name: "hydrate-me".to_string(),
            image_ref: "svc-img".to_string(),
            command: vec!["/bin/true".to_string()],
            ports: Vec::new(),
            env: Vec::new(),
            eager: false,
            run_dir: None,
            secrets: Vec::new(),
            configs: Vec::new(),
            healthcheck: None,
        };
        let cwd = prepare_service_cwd(&svc, &store).unwrap();
        assert!(
            cwd.join("marker.txt").exists(),
            "image_ref file must be hydrated"
        );
        assert_eq!(
            std::fs::read(cwd.join("marker.txt")).unwrap(),
            b"from-image"
        );
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn prepare_service_cwd_empty_ref_is_clean() {
        let store_tmp = TempDir::new().unwrap();
        let store = Store::open(store_tmp.path()).unwrap();
        let svc = ServiceSpec {
            name: "cmd-only".to_string(),
            image_ref: String::new(),
            command: vec!["/bin/true".to_string()],
            ports: Vec::new(),
            env: Vec::new(),
            eager: false,
            run_dir: None,
            secrets: Vec::new(),
            configs: Vec::new(),
            healthcheck: None,
        };
        let cwd = prepare_service_cwd(&svc, &store).unwrap();
        assert!(cwd.is_dir());
        assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
