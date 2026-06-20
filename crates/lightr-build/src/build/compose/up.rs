//! compose_up: start a compose stack.
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::collections::HashSet;
use std::path::PathBuf;

use super::lower_files::{FileSource, SourceKind};
use super::model::{Compose, ComposeHandle, Service, ServiceSpec, StackSpec};

/// CMP-P1-PROFILES: does a service's own profile list make it active?
///
/// Docker rule: a service with NO `profiles` is always active (the default); a
/// service WITH profiles is active only when at least one of them is in the
/// active set. `active` is the union of `--profile` flags and `COMPOSE_PROFILES`,
/// resolved at the call site. Empty `active` + no profiles ⇒ active (today's
/// behavior, behavior-preserving).
fn profile_selected(svc: &Service, active: &HashSet<&str>) -> bool {
    svc.profiles.is_empty() || svc.profiles.iter().any(|p| active.contains(p.as_str()))
}

/// CMP-P1-PROFILES: compute the ACTIVE service set for a compose stack, by name.
///
/// Two steps, matching Docker:
///  1. SELECT every service whose own profiles make it active (`profile_selected`).
///  2. AUTO-ACTIVATE dependencies (CMP-P0-DEPENDS interaction): if an active
///     service `depends_on` a profile-gated service, that dependency is pulled in
///     and started too — transitively (a pulled-in dep's own deps are pulled in
///     as well). A `depends_on` edge to a service not declared in the stack is
///     ignored (consistent with the supervisor's topo sort).
///
/// Behavior-preserving: with no profiles anywhere and an empty active set, step 1
/// selects every service and step 2 adds nothing ⇒ all services, as before.
pub(crate) fn active_service_names(c: &Compose, active: &HashSet<&str>) -> HashSet<String> {
    let mut selected: HashSet<String> = c
        .services
        .iter()
        .filter(|s| profile_selected(s, active))
        .map(|s| s.name.clone())
        .collect();

    // Transitive auto-activation of depends_on targets. Iterate to a fixpoint so
    // a chain `active -> profiled-dep -> profiled-dep-of-dep` is fully pulled in.
    let by_name: std::collections::HashMap<&str, &Service> =
        c.services.iter().map(|s| (s.name.as_str(), s)).collect();
    loop {
        let mut added = false;
        let frontier: Vec<String> = selected.iter().cloned().collect();
        for name in frontier {
            if let Some(svc) = by_name.get(name.as_str()) {
                for (dep_name, _cond) in &svc.depends_on {
                    if by_name.contains_key(dep_name.as_str()) && selected.insert(dep_name.clone())
                    {
                        added = true;
                    }
                }
            }
        }
        if !added {
            break;
        }
    }
    selected
}

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

/// WP-CMP-SECRETS-FULL: ingest the top-level `file:` sources of one kind
/// (`secret`/`config`) into the Store, registering each under its source name as
/// a store ref so the service-side `(name, source)` `StoreFile` resolves at run.
///
/// A `file:` source is snapshotted as a single-file tree (the source name is the
/// ref AND the file name inside the tree), matching how `lightr_run::secrets`
/// hydrates a ref into `<cwd>/.lightr/{secrets,configs}/<name>`. An `external:`
/// source is a no-op (the ref is assumed already registered — fail-closed at run
/// if absent, exactly like a `lightr run --secret name=missingref`). An `Other`
/// source (no `file:`/`external:`) is flagged once and skipped.
///
/// Fails CLOSED: a missing/unreadable `file:` path, or a source name that is not
/// a valid store-ref name, is an honest `Err` and no stack is spawned — a secret
/// the user declared must never be silently absent.
fn ingest_file_sources(store: &Store, sources: &[FileSource], kind: &str) -> Result<()> {
    for src in sources {
        match &src.kind {
            SourceKind::File(path) => ingest_one_file(store, &src.name, path, kind)?,
            SourceKind::External => {} // already-registered ref; resolved at run.
            SourceKind::Other => {
                eprintln!(
                    "lightr compose: top-level {kind} {:?} has neither `file:` nor \
                     `external:`; not ingested (refs to it will fail at run)",
                    src.name
                );
            }
        }
    }
    Ok(())
}

/// Ingest one `file:` source into the Store under `ref_name` (the source name).
/// Snapshots a single-file staging tree so `lightr_index::hydrate(dest, store,
/// ref_name)` materializes the bytes at run.
fn ingest_one_file(store: &Store, ref_name: &str, path: &str, kind: &str) -> Result<()> {
    let src_path = std::path::Path::new(path);
    if !src_path.is_file() {
        return Err(LightrError::InvalidManifest(format!(
            "compose {kind} source {ref_name:?}: file {path:?} does not exist or is not a file"
        )));
    }
    // Stage the file in a fresh tempdir under the file name == source name, then
    // snapshot the dir as the named ref (the snapshot/hydrate model is a tree).
    let staging = tempfile::tempdir().map_err(LightrError::Io)?;
    let dest = staging.path().join(ref_name);
    std::fs::copy(src_path, &dest).map_err(LightrError::Io)?;
    lightr_index::snapshot(staging.path(), store, ref_name).map_err(|e| {
        LightrError::InvalidManifest(format!(
            "compose {kind} source {ref_name:?}: store ingest failed: {e}"
        ))
    })?;
    Ok(())
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
/// `active_profiles` (CMP-P1-PROFILES) is the union of `--profile` flags and
/// `COMPOSE_PROFILES`, resolved at the call site (parallel-safe — no global env
/// read here). Only the ACTIVE service set (see [`active_service_names`]) is
/// written into the `StackSpec` and started/ordered by the supervisor;
/// profile-gated services whose profile is not active are excluded. An empty
/// `active_profiles` with no `profiles:` anywhere selects every service —
/// behavior-preserving (today's behavior).
///
/// Returns once the stack directory is written (ms).
pub fn compose_up(
    c: &Compose,
    store: &Store,
    ttl_secs: u64,
    project: &str,
    active_profiles: &[String],
) -> Result<ComposeHandle> {
    // WP-CMP-SECRETS-FULL: ingest every top-level `file:` secret/config source
    // into the Store under its source name as the ref, BEFORE the supervisor is
    // spawned — so a service's `(name, source)` `StoreFile` resolves at run
    // (`lightr_index::hydrate`) instead of failing on a missing ref. `external:`
    // sources are assumed already-registered; `Other` entries are flagged. No
    // top-level sources ⇒ no-op (behavior-preserving).
    ingest_file_sources(store, &c.secret_sources, "secret")?;
    ingest_file_sources(store, &c.config_sources, "config")?;
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

    // CMP-P1-PROFILES: restrict to the ACTIVE service set before building specs,
    // so the supervisor only sees (starts/orders) the services Docker would.
    let active_set: HashSet<&str> = active_profiles.iter().map(String::as_str).collect();
    let active_names = active_service_names(c, &active_set);

    let service_specs: Vec<ServiceSpec> = c
        .services
        .iter()
        .filter(|s| active_names.contains(&s.name))
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
            depends_on: s.depends_on.clone(),
            // CMP-LOWER-RUNCFG: carry the lowered run-config through the on-disk
            // spec so the supervisor can set them on the spawned RunSpec.
            working_dir: s.working_dir.clone(),
            user: s.user.clone(),
            restart: s.restart.clone(),
            // CMP-P1-DEPLOY: carry the deploy-derived caps + replica count so
            // the supervisor can apply/note them at the spawn site.
            mem_limit_bytes: s.mem_limit_bytes,
            cpu_limit_millis: s.cpu_limit_millis,
            replicas: s.replicas,
            // WP-CMP-CONFIG-LOWER: carry the lowered runtime config (init/tty/
            // privileged/cap_add/cap_drop/container_name) through the on-disk
            // spec so the supervisor can set them on the spawned RunSpec.
            init: s.init,
            tty: s.tty,
            privileged: s.privileged,
            cap_add: s.cap_add.clone(),
            cap_drop: s.cap_drop.clone(),
            container_name: s.container_name.clone(),
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

#[cfg(test)]
#[path = "up_tests.rs"]
mod tests;
