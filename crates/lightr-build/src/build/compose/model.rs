//! Compose data model: Service, Compose, ComposeHandle, StackSpec, ServiceSpec,
//! empty_service, parse_duration_secs.
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
}

pub struct Compose {
    pub services: Vec<Service>,
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
