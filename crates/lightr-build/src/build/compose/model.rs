//! Compose data model: Service, Compose, ComposeHandle, StackSpec, ServiceSpec,
//! empty_service, parse_duration_secs.
use super::lower_files::FileSource;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// CMP-P1-HEALTH-FULL: a lowered compose healthcheck —
/// `(cmd, interval_s, timeout_s, start_period_s, retries)`. These are exactly
/// the fields the runtime `lightr_run::healthcheck::Healthcheck` carries (RC-4
/// added timeout/start_period); the supervisor maps this tuple field-for-field.
pub type LoweredHealthcheck = (String, u64, u64, u64, u32);

/// CMP-P0-DEPENDS: the start-order condition on a `depends_on` edge.
///
/// Mirrors compose's three conditions verbatim. `Started` is the short-form
/// default (`depends_on: [db]`): the dependency need only be SPAWNED before the
/// dependent starts. `Healthy` waits for the dependency's healthcheck verdict to
/// report `healthy`. `Completed` (`service_completed_successfully`) waits for the
/// dependency to exit 0. Serialized as the compose condition string so the
/// on-disk `StackSpec` is self-describing and round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepCondition {
    #[serde(rename = "service_started")]
    Started,
    #[serde(rename = "service_healthy")]
    Healthy,
    #[serde(rename = "service_completed_successfully")]
    Completed,
}

impl DepCondition {
    /// Parse a compose `condition:` string. Unknown/absent ⇒ the short-form
    /// default `service_started` (Docker-faithful: the short list form and any
    /// long entry without a condition both mean "dependency started").
    pub(crate) fn parse(s: Option<&str>) -> DepCondition {
        match s {
            Some("service_healthy") => DepCondition::Healthy,
            Some("service_completed_successfully") => DepCondition::Completed,
            // `service_started` and any unrecognized value fall back to started.
            _ => DepCondition::Started,
        }
    }
}

/// CMP-P0-DEPENDS: one start-order dependency edge — `(dep_service, condition)`.
pub type DepEdge = (String, DepCondition);

pub struct Service {
    pub name: String,
    pub image_ref: String,
    pub command: Option<Vec<String>>,
    pub ports: Vec<(u16, u16)>,
    pub env: Vec<(String, String)>,
    pub eager: bool,
    /// F-309: store-backed secrets, each `(name, ref)`.
    pub secrets: Vec<(String, String)>,
    /// F-309: store-backed configs, each `(name, ref)`.
    pub configs: Vec<(String, String)>,
    /// CMP-P1-HEALTH-FULL: optional healthcheck — see [`LoweredHealthcheck`].
    pub healthcheck: Option<LoweredHealthcheck>,
    /// CMP-P0-DEPENDS: start-order dependency edges (`dep -> condition`). Empty
    /// for a service with no `depends_on` (behavior-preserving — supervisor
    /// start order is then the declaration order, exactly as before).
    pub depends_on: Vec<DepEdge>,
    /// CMP-LOWER-RUNCFG: compose `working_dir`, lowered into `RunSpec.workdir`
    /// (WP-RC-WORKDIR). `None` ⇒ run in the service cwd (today's behavior).
    pub working_dir: Option<String>,
    /// CMP-LOWER-RUNCFG: compose `user`, lowered into `RunSpec.user`
    /// (WP-RC-USER). `None` ⇒ run as the current user (today's behavior).
    pub user: Option<String>,
    /// CMP-LOWER-RUNCFG: compose `restart`, lowered into `RunSpec.restart`
    /// (WP-RC-RESTART). `None` ⇒ `no` policy (today's behavior). CMP-P1-DEPLOY:
    /// `deploy.restart_policy.condition` OVERRIDES this when both are set
    /// (compose precedence — `lower_deploy` writes the mapped policy here).
    pub restart: Option<String>,
    /// CMP-P1-DEPLOY: `deploy.resources.limits.memory`, parsed to bytes with the
    /// SAME grammar as `lightr run --memory` (`ResourceLimits::parse`). `None` ⇒
    /// unlimited (today's behavior). NOTE: carried through the on-disk spec but
    /// not yet ENFORCED on the detached compose spawn path — the limits channel
    /// (`spawn_detached_engine` / `SpecOnDisk`) lives in `lightr-run`, which this
    /// WP does not own; honest once-note at the spawn site (follow-up WP).
    pub mem_limit_bytes: Option<u64>,
    /// CMP-P1-DEPLOY: `deploy.resources.limits.cpus`, parsed to milli-CPUs with
    /// the SAME grammar as `lightr run --cpus` (1000 = one core). See
    /// [`Service::mem_limit_bytes`] for the not-yet-enforced caveat.
    pub cpu_limit_millis: Option<u64>,
    /// CMP-P1-DEPLOY: `deploy.replicas` when > 1. OUT OF SCOPE for this WP
    /// (multi-instance spawn is a separate WP); carried only so the spawn site
    /// can emit an honest "not yet honored" note instead of silently ignoring it.
    pub replicas: Option<u32>,
    /// CMP-P1-PROFILES: the service's `profiles: [...]` list (verbatim from the
    /// compose file). A service with a NON-EMPTY list is started only when one of
    /// these profiles is ACTIVE (`--profile`/`COMPOSE_PROFILES`); an EMPTY list
    /// means the service is always active (the default / today's behavior). The
    /// activation filter lives at the `compose up` call site (`up.rs`), not here.
    pub profiles: Vec<String>,
    /// WP-CMP-CONFIG-LOWER: compose `init`, lowered into `RunSpec.init` (PID-1
    /// reaper). `false` (the absent default) ⇒ no init process (today's behavior).
    pub init: bool,
    /// WP-CMP-CONFIG-LOWER: compose `tty`, lowered into `RunSpec.tty`. `false`
    /// (absent) ⇒ no TTY (today's behavior).
    pub tty: bool,
    /// WP-CMP-CONFIG-LOWER: compose `privileged`, lowered into
    /// `RunSpec.privileged`. `false` (absent) ⇒ unprivileged (today's behavior).
    pub privileged: bool,
    /// WP-CMP-CONFIG-LOWER: compose `cap_add`, lowered into `RunSpec.cap_add`
    /// (Linux capabilities to add). Empty (absent) ⇒ default cap set (today's
    /// behavior).
    pub cap_add: Vec<String>,
    /// WP-CMP-CONFIG-LOWER: compose `cap_drop`, lowered into `RunSpec.cap_drop`
    /// (Linux capabilities to drop). Empty (absent) ⇒ default cap set (today's
    /// behavior).
    pub cap_drop: Vec<String>,
    /// WP-CMP-CONFIG-LOWER: compose `container_name`, the explicit run-name
    /// override. `None` (absent) ⇒ Lightr derives the run name from the service
    /// name (today's behavior). Only the materialized run dir name is affected;
    /// the compose service name (depends_on edges, discovery keys) is unchanged.
    pub container_name: Option<String>,
    /// WP-CMP-NET: the service's compose `networks:` attachments, each lowered to
    /// `(network_name, aliases)` in declaration order (short list ⇒ empty aliases;
    /// long map ⇒ the declared per-network `aliases`). EMPTY (the absent default)
    /// ⇒ the service declares NO network and runs NATIVE with loopback+env
    /// discovery, exactly as today (behavior-preserving). A NON-EMPTY list routes
    /// the service to the `vz` engine + the shared L2 switch (DNS-by-service-name).
    /// The compose project name is prepended to each entry (`<project>_<network>`)
    /// at the supervisor's spawn site, matching Docker's per-project network
    /// namespacing — so it is held UN-prefixed here (the lowering has no project).
    pub networks: Vec<(String, Vec<String>)>,
    /// WP-A: compose `entrypoint`, lowered into `RunSpec.entrypoint` (override the
    /// image entrypoint; the run side prepends it to `command`). `None` (absent)
    /// ⇒ no override (today's behavior). Exec-form list as-is; a shell string
    /// becomes `["/bin/sh", "-c", <str>]` (mirrors `lower_command`).
    pub entrypoint: Option<Vec<String>>,
    /// WP-A: compose `extra_hosts`, lowered into `RunSpec.add_host` as raw
    /// `"host:ip"` strings. Empty (absent) ⇒ no extra `/etc/hosts` entries
    /// (today's behavior).
    pub extra_hosts: Vec<String>,
    /// WP-A: compose `stop_signal`, lowered into `RunSpec.stop_signal` (the signal
    /// `lightr stop` sends before SIGKILL). `None` (absent) ⇒ SIGTERM (today's
    /// behavior). The signal STRING is transcribed as-is; its parsing is the run
    /// side's law.
    pub stop_signal: Option<String>,
    /// WP-A: compose `hostname`, lowered into `RunSpec.hostname`. `None` (absent)
    /// ⇒ the run derives no explicit hostname (today's behavior).
    pub hostname: Option<String>,
}

pub struct Compose {
    pub services: Vec<Service>,
    /// WP-CMP-SECRETS-FULL (model touch — FLAGGED): the top-level `secrets:`
    /// source map, lowered to typed [`FileSource`]s, so the up-path (`up.rs`,
    /// which holds the Store) can INGEST each `file:` source into the Store under
    /// its name as the ref — making a service's `(name, source)` `StoreFile`
    /// resolve at run. Empty ⇒ no top-level secrets (behavior-preserving).
    pub secret_sources: Vec<FileSource>,
    /// WP-CMP-SECRETS-FULL: the top-level `configs:` source map — counterpart of
    /// [`Self::secret_sources`].
    pub config_sources: Vec<FileSource>,
}

/// The project name a pre-CMP-P1-PROJECT `spec.json` is read back as (it had
/// no `project` field). Matches Docker's "default" fallback so old stacks
/// behave as before under project-aware `compose down`.
fn default_project() -> String {
    "default".to_string()
}

/// On-disk spec written by `compose_up` for the supervisor process.
#[derive(Serialize, Deserialize)]
pub struct StackSpec {
    pub ttl_secs: u64,
    pub created_at_unix: u64,
    /// CMP-P1-PROJECT: the project name namespacing this stack
    /// (precedence cli>env>`name:`>basename, resolved at `compose up`).
    /// `#[serde(default = ...)]` keeps pre-existing stack specs (no `project`
    /// field) loading as `"default"`.
    #[serde(default = "default_project")]
    pub project: String,
    /// pid of the supervisor process (written after fork)
    pub supervisor_pid: Option<u32>,
    pub services: Vec<ServiceSpec>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ServiceSpec {
    pub name: String,
    pub image_ref: String,
    pub command: Vec<String>,
    pub ports: Vec<(u16, u16)>,
    pub env: Vec<(String, String)>,
    pub eager: bool,
    /// Run dir if started (populated by supervisor)
    pub run_dir: Option<String>,
    /// F-309: store-backed secrets `(name, ref)`.
    #[serde(default)]
    pub secrets: Vec<(String, String)>,
    /// F-309: store-backed configs `(name, ref)`.
    #[serde(default)]
    pub configs: Vec<(String, String)>,
    /// CMP-P1-HEALTH-FULL: optional healthcheck — see [`LoweredHealthcheck`].
    /// `#[serde(default)]` keeps pre-existing stack specs (no healthcheck field)
    /// loading as `None`.
    #[serde(default)]
    pub healthcheck: Option<LoweredHealthcheck>,
    /// CMP-P0-DEPENDS: start-order dependency edges (`dep -> condition`). The
    /// supervisor topo-sorts on these to start deps before dependents and to
    /// gate on each edge's condition. `#[serde(default)]` keeps pre-existing
    /// stack specs (no `depends_on` field) loading as empty (= today's order).
    #[serde(default)]
    pub depends_on: Vec<DepEdge>,
    /// CMP-LOWER-RUNCFG: compose `working_dir` → `RunSpec.workdir`. `#[serde(
    /// default)]` keeps pre-existing stack specs (no field) loading as `None`.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// CMP-LOWER-RUNCFG: compose `user` → `RunSpec.user`. serde-default = None.
    #[serde(default)]
    pub user: Option<String>,
    /// CMP-LOWER-RUNCFG: compose `restart` → `RunSpec.restart`. serde-default =
    /// None. CMP-P1-DEPLOY: holds the deploy.restart_policy-derived policy when
    /// the deploy block sets one (it wins over a top-level `restart:`).
    #[serde(default)]
    pub restart: Option<String>,
    /// CMP-P1-DEPLOY: `deploy.resources.limits.memory` in bytes (parsed like
    /// `lightr run --memory`). serde-default = None (pre-existing specs load
    /// unchanged). Carried to the supervisor; not yet enforced on the detached
    /// path (limits channel is in `lightr-run`, a follow-up WP).
    #[serde(default)]
    pub mem_limit_bytes: Option<u64>,
    /// CMP-P1-DEPLOY: `deploy.resources.limits.cpus` in milli-CPUs (parsed like
    /// `lightr run --cpus`). serde-default = None. Same not-yet-enforced caveat.
    #[serde(default)]
    pub cpu_limit_millis: Option<u64>,
    /// CMP-P1-DEPLOY: `deploy.replicas` (when > 1). serde-default = None.
    /// OUT OF SCOPE — carried only for the honest "not yet honored" note.
    #[serde(default)]
    pub replicas: Option<u32>,
    /// WP-CMP-CONFIG-LOWER: compose `init` → `RunSpec.init`. `#[serde(default)]`
    /// keeps pre-existing stack specs (no field) loading as `false`.
    #[serde(default)]
    pub init: bool,
    /// WP-CMP-CONFIG-LOWER: compose `tty` → `RunSpec.tty`. serde-default = false.
    #[serde(default)]
    pub tty: bool,
    /// WP-CMP-CONFIG-LOWER: compose `privileged` → `RunSpec.privileged`.
    /// serde-default = false.
    #[serde(default)]
    pub privileged: bool,
    /// WP-CMP-CONFIG-LOWER: compose `cap_add` → `RunSpec.cap_add`. serde-default
    /// = empty.
    #[serde(default)]
    pub cap_add: Vec<String>,
    /// WP-CMP-CONFIG-LOWER: compose `cap_drop` → `RunSpec.cap_drop`. serde-default
    /// = empty.
    #[serde(default)]
    pub cap_drop: Vec<String>,
    /// WP-CMP-CONFIG-LOWER: compose `container_name` → the run-dir name override
    /// at the spawn site. serde-default = None (run name derived from the service
    /// name, today's behavior).
    #[serde(default)]
    pub container_name: Option<String>,
    /// WP-CMP-NET: the service's compose `networks:` as `(network_name, aliases)`
    /// (un-prefixed; the supervisor prepends `<project>_`). EMPTY ⇒ the service
    /// declares no network ⇒ NATIVE spawn (today's behavior). NON-EMPTY ⇒ the
    /// supervisor routes the service to the `vz` engine and sets `RunSpec.network`
    /// (= `<project>_<first network>`) so C9's svz path joins the per-network
    /// registry + attaches the shared L2 switch (DNS-by-service-name). `#[serde(
    /// default)]` keeps pre-existing stack specs (no `networks` field) loading as
    /// empty ⇒ native, byte-identical.
    #[serde(default)]
    pub networks: Vec<(String, Vec<String>)>,
    /// WP-A: compose `entrypoint` → `RunSpec.entrypoint`. `#[serde(default)]` keeps
    /// pre-existing stack specs (no field) loading as `None`.
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    /// WP-A: compose `extra_hosts` → `RunSpec.add_host` (`"host:ip"` strings).
    /// serde-default = empty.
    #[serde(default)]
    pub extra_hosts: Vec<String>,
    /// WP-A: compose `stop_signal` → `RunSpec.stop_signal`. serde-default = None.
    #[serde(default)]
    pub stop_signal: Option<String>,
    /// WP-A: compose `hostname` → `RunSpec.hostname`. serde-default = None.
    #[serde(default)]
    pub hostname: Option<String>,
}

pub struct ComposeHandle {
    pub stack_dir: PathBuf,
    pub services: Vec<String>,
}

/// A fresh service with the given name and all-empty fields.
pub(crate) fn empty_service(name: String) -> Service {
    Service {
        name,
        image_ref: String::new(),
        command: None,
        ports: Vec::new(),
        env: Vec::new(),
        eager: false,
        secrets: Vec::new(),
        configs: Vec::new(),
        healthcheck: None,
        depends_on: Vec::new(),
        working_dir: None,
        user: None,
        restart: None,
        mem_limit_bytes: None,
        cpu_limit_millis: None,
        replicas: None,
        profiles: Vec::new(),
        init: false,
        tty: false,
        privileged: false,
        cap_add: Vec::new(),
        cap_drop: Vec::new(),
        container_name: None,
        networks: Vec::new(),
        entrypoint: None,
        extra_hosts: Vec::new(),
        stop_signal: None,
        hostname: None,
    }
}

/// Parse a Docker-compose duration into whole seconds.
/// Accepts `"30s"`, `"1m"`, `"2m30s"` (s/m/h suffixes), or a bare integer.
pub(crate) fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut saw_unit = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else {
            let n: u64 = num.parse().ok()?;
            num.clear();
            let mult = match ch {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                _ => return None,
            };
            total = total.checked_add(n.checked_mul(mult)?)?;
            saw_unit = true;
        }
    }
    if !num.is_empty() || !saw_unit {
        return None;
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_secs_forms() {
        assert_eq!(parse_duration_secs("30"), Some(30));
        assert_eq!(parse_duration_secs("30s"), Some(30));
        assert_eq!(parse_duration_secs("1m"), Some(60));
        assert_eq!(parse_duration_secs("2m30s"), Some(150));
        assert_eq!(parse_duration_secs("1h"), Some(3600));
        assert_eq!(parse_duration_secs(""), None);
        assert_eq!(parse_duration_secs("30x"), None);
        assert_eq!(parse_duration_secs("abc"), None);
        assert_eq!(
            parse_duration_secs("10s5"),
            None,
            "trailing unit-less number"
        );
    }
}
