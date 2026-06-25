use super::error::{LightrError, Result};

// ── ResourceLimits ────────────────────────────────────────────────────────────
// build-spec-parity.md §A0.1 / F-203. Pure type + parser only (core forbids
// unsafe); the setrlimit/cgroup/VM application lives in lightr-run + lightr-engine.
// NOT part of the memo key (resource caps don't change deterministic output).

/// Resource caps for a run. `None` = unlimited (parity default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceLimits {
    pub memory_bytes: Option<u64>,
    /// CPU as milli-CPUs: 1000 = one full core, 500 = half. `None` = unlimited.
    pub cpu_millis: Option<u64>,
    /// Max process/thread count (Docker `--pids-limit`, cgroup v2 `pids.max`).
    /// `None` = unlimited. Enforced ONLY on the `ns` engine (cgroup v2); native
    /// and vz honest-error when this is set (no per-container pids cap there).
    pub pids_max: Option<u64>,
}

impl ResourceLimits {
    pub fn is_unlimited(&self) -> bool {
        self.memory_bytes.is_none() && self.cpu_millis.is_none() && self.pids_max.is_none()
    }

    /// Parse Docker-style strings. memory: "512m" "1g" "2048k" "1073741824".
    /// cpus: "0.5" "2" "1.5". Fail closed on malformed input.
    pub fn parse(memory: Option<&str>, cpus: Option<&str>) -> Result<Self> {
        let memory_bytes = match memory {
            None => None,
            Some(s) => Some(parse_memory(s)?),
        };
        let cpu_millis = match cpus {
            None => None,
            Some(s) => Some(parse_cpus(s)?),
        };
        Ok(ResourceLimits {
            memory_bytes,
            cpu_millis,
            pids_max: None,
        })
    }

    /// Builder: fold in the `--pids-limit` value (Docker semantics). A value
    /// `<= 0` (Docker's "unlimited") clears the cap to `None`; a positive value
    /// becomes the cgroup `pids.max`. Keeps `parse` signature stable so existing
    /// callers are untouched.
    pub fn with_pids(mut self, pids: Option<i64>) -> Self {
        self.pids_max = pids.filter(|n| *n > 0).map(|n| n as u64);
        self
    }
}

fn parse_memory(s: &str) -> Result<u64> {
    let s = s.trim();
    let last = s
        .chars()
        .last()
        .ok_or_else(|| LightrError::InvalidRef("empty memory limit".to_string()))?;
    let (num, mult): (&str, u64) = match last {
        'k' | 'K' => (&s[..s.len() - 1], 1024),
        'm' | 'M' => (&s[..s.len() - 1], 1024 * 1024),
        'g' | 'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        '0'..='9' => (s, 1),
        _ => {
            return Err(LightrError::InvalidRef(format!(
                "invalid memory limit: {s}"
            )))
        }
    };
    let val: u64 = num
        .trim()
        .parse()
        .map_err(|_| LightrError::InvalidRef(format!("invalid memory limit: {s}")))?;
    if val == 0 {
        return Err(LightrError::InvalidRef(
            "memory limit must be > 0".to_string(),
        ));
    }
    val.checked_mul(mult)
        .ok_or_else(|| LightrError::InvalidRef(format!("memory limit overflow: {s}")))
}

fn parse_cpus(s: &str) -> Result<u64> {
    let f: f64 = s
        .trim()
        .parse()
        .map_err(|_| LightrError::InvalidRef(format!("invalid cpus: {s}")))?;
    if !(f.is_finite() && f > 0.0) {
        return Err(LightrError::InvalidRef(format!("cpus must be > 0: {s}")));
    }
    Ok((f * 1000.0).round() as u64)
}

#[cfg(test)]
mod resource_limits_tests {
    use super::*;

    #[test]
    fn parse_memory_suffixes() {
        assert_eq!(
            ResourceLimits::parse(Some("512m"), None)
                .unwrap()
                .memory_bytes,
            Some(512 * 1024 * 1024)
        );
        assert_eq!(
            ResourceLimits::parse(Some("1g"), None)
                .unwrap()
                .memory_bytes,
            Some(1024 * 1024 * 1024)
        );
        assert_eq!(
            ResourceLimits::parse(Some("2048k"), None)
                .unwrap()
                .memory_bytes,
            Some(2048 * 1024)
        );
        assert_eq!(
            ResourceLimits::parse(Some("1073741824"), None)
                .unwrap()
                .memory_bytes,
            Some(1073741824)
        );
    }

    #[test]
    fn parse_cpus_to_millis() {
        assert_eq!(
            ResourceLimits::parse(None, Some("0.5")).unwrap().cpu_millis,
            Some(500)
        );
        assert_eq!(
            ResourceLimits::parse(None, Some("2")).unwrap().cpu_millis,
            Some(2000)
        );
        assert_eq!(
            ResourceLimits::parse(None, Some("1.5")).unwrap().cpu_millis,
            Some(1500)
        );
    }

    #[test]
    fn fail_closed_on_garbage() {
        assert!(ResourceLimits::parse(Some("abc"), None).is_err());
        assert!(ResourceLimits::parse(Some("0"), None).is_err());
        assert!(ResourceLimits::parse(Some("-5"), None).is_err());
        assert!(ResourceLimits::parse(None, Some("0")).is_err());
        assert!(ResourceLimits::parse(None, Some("-1")).is_err());
        assert!(ResourceLimits::parse(None, Some("x")).is_err());
    }

    #[test]
    fn default_is_unlimited() {
        assert!(ResourceLimits::default().is_unlimited());
        assert!(!ResourceLimits::parse(Some("1m"), None)
            .unwrap()
            .is_unlimited());
    }

    #[test]
    fn with_pids_sets_and_clears() {
        // Positive ⇒ cap set.
        assert_eq!(
            ResourceLimits::default().with_pids(Some(16)).pids_max,
            Some(16)
        );
        // None ⇒ no cap.
        assert_eq!(ResourceLimits::default().with_pids(None).pids_max, None);
        // Docker's "unlimited" (<= 0) ⇒ cleared to None (never a 0 cap).
        assert_eq!(ResourceLimits::default().with_pids(Some(0)).pids_max, None);
        assert_eq!(ResourceLimits::default().with_pids(Some(-1)).pids_max, None);
        // with_pids does not disturb the other caps.
        let l = ResourceLimits::parse(Some("64m"), Some("0.5"))
            .unwrap()
            .with_pids(Some(32));
        assert_eq!(l.memory_bytes, Some(64 * 1024 * 1024));
        assert_eq!(l.cpu_millis, Some(500));
        assert_eq!(l.pids_max, Some(32));
    }

    #[test]
    fn is_unlimited_reflects_pids() {
        // pids alone makes a spec NON-unlimited.
        assert!(!ResourceLimits::default().with_pids(Some(8)).is_unlimited());
        // clearing it back to None is unlimited again.
        assert!(ResourceLimits::default().with_pids(Some(-1)).is_unlimited());
    }
}
