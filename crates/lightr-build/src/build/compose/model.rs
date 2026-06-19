//! Compose data model: Service, Compose, ComposeHandle, StackSpec, ServiceSpec,
//! empty_service, parse_duration_secs.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    /// F-309: optional healthcheck `(cmd, interval_s, retries)`.
    pub healthcheck: Option<(String, u64, u32)>,
}

pub struct Compose {
    pub services: Vec<Service>,
}

/// On-disk spec written by `compose_up` for the supervisor process.
#[derive(Serialize, Deserialize)]
pub struct StackSpec {
    pub ttl_secs: u64,
    pub created_at_unix: u64,
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
    /// F-309: optional healthcheck `(cmd, interval_s, retries)`.
    #[serde(default)]
    pub healthcheck: Option<(String, u64, u32)>,
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
