//! compose_up: start a compose stack.
use lightr_core::{LightrError, Result};
use lightr_store::Store;
use std::collections::HashSet;
use std::path::PathBuf;

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
mod tests {
    use super::*;
    use crate::build::compose::model::{empty_service, DepCondition};

    /// A service with the given name + profile list (no deps).
    fn svc(name: &str, profiles: &[&str]) -> Service {
        let mut s = empty_service(name.to_string());
        s.profiles = profiles.iter().map(|p| p.to_string()).collect();
        s
    }

    /// `active` set from a slice of profile names.
    fn active<'a>(names: &[&'a str]) -> HashSet<&'a str> {
        names.iter().copied().collect()
    }

    fn names_of(c: &Compose, act: &HashSet<&str>) -> Vec<String> {
        let mut v: Vec<String> = active_service_names(c, act).into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn no_profiles_no_active_all_services_active() {
        // Behavior-preserving: nothing profiled, no --profile ⇒ every service.
        let c = Compose {
            services: vec![svc("web", &[]), svc("db", &[])],
        };
        assert_eq!(names_of(&c, &active(&[])), vec!["db", "web"]);
    }

    #[test]
    fn profiled_service_excluded_when_profile_inactive() {
        let c = Compose {
            services: vec![svc("web", &[]), svc("debug", &["dev"])],
        };
        // `dev` not active ⇒ `debug` excluded, `web` (no profiles) stays.
        assert_eq!(names_of(&c, &active(&[])), vec!["web"]);
    }

    #[test]
    fn profiled_service_included_when_profile_active() {
        let c = Compose {
            services: vec![svc("web", &[]), svc("debug", &["dev"])],
        };
        assert_eq!(names_of(&c, &active(&["dev"])), vec!["debug", "web"]);
    }

    #[test]
    fn one_of_several_profiles_activates() {
        let c = Compose {
            services: vec![svc("svc", &["a", "b"])],
        };
        // Any one matching profile activates the service.
        assert_eq!(names_of(&c, &active(&["b"])), vec!["svc"]);
        assert!(active_service_names(&c, &active(&["c"])).is_empty());
    }

    #[test]
    fn no_profile_service_always_active() {
        let c = Compose {
            services: vec![svc("web", &[])],
        };
        // Even with unrelated profiles active, a no-profile service stays in.
        assert_eq!(names_of(&c, &active(&["dev", "prod"])), vec!["web"]);
    }

    #[test]
    fn active_service_pulls_in_profiled_dependency() {
        // Docker rule: an active service's depends_on target auto-activates even
        // if that target is profile-gated and its profile is not active.
        let mut web = svc("web", &[]);
        web.depends_on = vec![("db".to_string(), DepCondition::Started)];
        let db = svc("db", &["storage"]);
        let c = Compose {
            services: vec![web, db],
        };
        // `storage` is NOT active, yet `db` is pulled in by `web`'s depends_on.
        assert_eq!(names_of(&c, &active(&[])), vec!["db", "web"]);
    }

    #[test]
    fn auto_activation_is_transitive() {
        // active web -> profiled api -> profiled db: all pulled in.
        let mut web = svc("web", &[]);
        web.depends_on = vec![("api".to_string(), DepCondition::Started)];
        let mut api = svc("api", &["backend"]);
        api.depends_on = vec![("db".to_string(), DepCondition::Started)];
        let db = svc("db", &["storage"]);
        let c = Compose {
            services: vec![web, api, db],
        };
        assert_eq!(names_of(&c, &active(&[])), vec!["api", "db", "web"]);
    }

    #[test]
    fn inactive_service_does_not_pull_in_its_deps() {
        // `debug` (profile `dev`, inactive) depends_on `db` (profile `storage`).
        // Neither is active and `debug` is not selected, so nothing is pulled in.
        let mut debug = svc("debug", &["dev"]);
        debug.depends_on = vec![("db".to_string(), DepCondition::Started)];
        let db = svc("db", &["storage"]);
        let c = Compose {
            services: vec![svc("web", &[]), debug, db],
        };
        assert_eq!(names_of(&c, &active(&[])), vec!["web"]);
    }
}
