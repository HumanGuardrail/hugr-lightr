//! compose_up: start a compose stack.
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::path::PathBuf;

use super::model::{Compose, ComposeHandle, ServiceSpec, StackSpec};

fn lightr_home() -> PathBuf {
    if let Ok(h) = std::env::var("LIGHTR_HOME") {
        PathBuf::from(h)
    } else {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));
        home.join(".lightr")
    }
}

pub(crate) fn lightr_home_pub() -> PathBuf {
    lightr_home()
}

/// Start a compose stack.
///
/// - Creates `$LIGHTR_HOME/compose/<nanos-pid>/spec.json`.
/// - Spawns a detached `lightr __compose-supervise <stack_dir>` process.
/// - Eager services are noted in the spec; the supervisor starts them immediately.
/// - Lazy services: the supervisor binds their host ports and starts the service
///   on the first incoming connection.
///
/// `project` is the resolved compose project name (CMP-P1-PROJECT —
/// precedence cli>env>`name:`>basename, sanitized to Docker's grammar at the
/// call site). It is recorded in the `StackSpec` so `compose down -p <name>`
/// can target exactly this stack's project and two projects never collide.
///
/// Returns once the stack directory is written (ms).
pub fn compose_up(
    c: &Compose,
    store: &Store,
    ttl_secs: u64,
    project: &str,
) -> Result<ComposeHandle> {
    let _ = store; // store reserved for future hydrate-before-spawn path
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();

    let stack_dir = lightr_home()
        .join("compose")
        .join(format!("{now_nanos}-{pid}"));
    std::fs::create_dir_all(&stack_dir).map_err(LightrError::Io)?;

    let created_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let service_specs: Vec<ServiceSpec> = c
        .services
        .iter()
        .map(|s| ServiceSpec {
            name: s.name.clone(),
            image_ref: s.image_ref.clone(),
            command: s.command.clone().unwrap_or_default(),
            ports: s.ports.clone(),
            env: s.env.clone(),
            eager: s.eager,
            run_dir: None,
            secrets: s.secrets.clone(),
            configs: s.configs.clone(),
            healthcheck: s.healthcheck.clone(),
        })
        .collect();

    let spec = StackSpec {
        ttl_secs,
        created_at_unix,
        project: project.to_string(),
        supervisor_pid: None,
        services: service_specs.clone(),
    };

    let spec_bytes = serde_json::to_vec_pretty(&spec)
        .map_err(|e| LightrError::InvalidManifest(format!("stack spec serialize: {e}")))?;
    let spec_path = stack_dir.join("spec.json");
    std::fs::write(&spec_path, &spec_bytes).map_err(LightrError::Io)?;

    let exe = std::env::current_exe().map_err(LightrError::Io)?;
    let stack_str = stack_dir.to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["__compose-supervise", &stack_str]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    cmd.spawn().map_err(LightrError::Io)?;

    let service_names: Vec<String> = service_specs.iter().map(|s| s.name.clone()).collect();
    Ok(ComposeHandle {
        stack_dir,
        services: service_names,
    })
}
