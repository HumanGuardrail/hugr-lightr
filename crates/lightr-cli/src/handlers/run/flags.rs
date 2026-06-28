//! `lightr run` flag parsing + value types (skeleton-split from `mod.rs`).
//!
//! Behavior-preserving extraction of the cohesive parsing preamble: the JSON
//! report struct, the `--mount`/`--secret`/`--config`/`-p` value parsers, and
//! the `--health-*` flag bundle. No logic change — `run()` (in `mod.rs`) calls
//! these exactly as before; tests reach them via `super::` re-exports.

use lightr_core::validate_ref_name;
use lightr_run::healthcheck::Healthcheck;
use lightr_run::{Mount, StoreFile};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct RunJson {
    pub(crate) key: String,
    pub(crate) hit: bool,
    pub(crate) exit_code: i32,
}

/// Parse a raw "ref:target" mount string into (ref_name, target).
/// Returns Err(exit_code) on validation failure (already printed to stderr).
pub(crate) fn parse_mount(raw: &str) -> Result<Mount, i32> {
    // Split on FIRST ':' only
    let colon = raw.find(':').ok_or_else(|| {
        eprintln!("lightr: invalid --mount value (missing ':'): {raw}");
        2i32
    })?;
    let ref_name = &raw[..colon];
    let target = &raw[colon + 1..];

    // Validate ref name
    if let Err(e) = validate_ref_name(ref_name) {
        eprintln!("lightr: invalid mount ref name: {e}");
        return Err(2);
    }

    // Validate target is relative (not absolute)
    if target.starts_with('/') {
        eprintln!("lightr: mount target must be relative, got: {target}");
        return Err(2);
    }

    Ok(Mount {
        ref_name: ref_name.to_string(),
        target: target.to_string(),
    })
}

/// Parse a raw "NAME=REF" secret/config string into a `StoreFile`.
/// Returns Err(exit_code) on a missing '=' (already printed to stderr).
pub(crate) fn parse_store_file(raw: &str, kind: &str) -> Result<StoreFile, i32> {
    let eq = raw.find('=').ok_or_else(|| {
        eprintln!("lightr: invalid --{kind} value (missing '='): {raw}");
        2i32
    })?;
    let name = &raw[..eq];
    let ref_name = &raw[eq + 1..];
    if name.is_empty() || ref_name.is_empty() {
        eprintln!("lightr: invalid --{kind} value (expected NAME=REF): {raw}");
        return Err(2);
    }
    Ok(StoreFile {
        name: name.to_string(),
        ref_name: ref_name.to_string(),
    })
}

#[path = "flags_publish.rs"]
pub(crate) mod publish;
// WP-B2: the run path now consumes the range-aware `parse_publish_spec` and the
// `-P` `synth_publish_all` builder directly via `publish::…`. The single-port
// `parse_publish` wrapper is retained for its single-vs-range contract test only,
// so its re-export is `#[cfg(test)]` (no unused-import warning in the binary).
#[cfg(test)]
pub(crate) use publish::parse_publish;

/// Parse a raw `--label`/`-l` value `KEY=VAL` into a `(key, value)` pair
/// (WP-RC-FLAGS). A label is metadata only — it has no exec effect; it is
/// recorded in spec.json and surfaced by `lightr inspect`. The key must be
/// non-empty; the value may be empty (docker accepts `--label key=`). Splits on
/// the FIRST `=`. On a missing `=` or empty key, prints to stderr + `Err(2)`
/// (mirrors `parse_store_file`).
pub(crate) fn parse_label(raw: &str) -> Result<(String, String), i32> {
    let eq = raw.find('=').ok_or_else(|| {
        eprintln!("lightr: invalid --label value (expected KEY=VAL): {raw}");
        2i32
    })?;
    let key = &raw[..eq];
    let value = &raw[eq + 1..];
    if key.is_empty() {
        eprintln!("lightr: invalid --label value (empty key): {raw}");
        return Err(2);
    }
    Ok((key.to_string(), value.to_string()))
}

/// Parse a raw `--shm-size` value into bytes (WP-RC-FLAGS). Docker-style:
/// `64m`, `1g`, `2048k`, or bare bytes. Must be `> 0`. On malformed input
/// prints to stderr + `Err(2)` (mirrors `parse_publish`). The `b` suffix is
/// accepted as bytes (docker's `--shm-size=64m` grammar permits a bare unit).
pub(crate) fn parse_shm_size(raw: &str) -> Result<u64, i32> {
    let s = raw.trim();
    let bad = |s: &str| {
        eprintln!("lightr: invalid --shm-size value (expected e.g. 64m, 1g, or bytes): {s}");
        2i32
    };
    let last = s.chars().last().ok_or_else(|| bad(s))?;
    let (num, mult): (&str, u64) = match last {
        'k' | 'K' => (&s[..s.len() - 1], 1024),
        'm' | 'M' => (&s[..s.len() - 1], 1024 * 1024),
        'g' | 'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        'b' | 'B' => (&s[..s.len() - 1], 1),
        '0'..='9' => (s, 1),
        _ => return Err(bad(s)),
    };
    let val: u64 = num.trim().parse().map_err(|_| bad(s))?;
    if val == 0 {
        eprintln!("lightr: invalid --shm-size value (must be > 0): {s}");
        return Err(2);
    }
    val.checked_mul(mult).ok_or_else(|| {
        eprintln!("lightr: --shm-size overflow: {s}");
        2i32
    })
}

/// The 11 WP-RC-FLAGS run-config flags, bundled as RAW clap values (WP-RC-FLAGS).
/// Built from the parsed `Cmd` in dispatch and lowered to a [`RcConfig`] by
/// [`RawRcFlags::resolve`], which parses `--label` (KEY=VAL) and `--shm-size`
/// (size string). Bundling keeps `run()`'s arity flat. RUNTIME-ONLY — none of
/// these enters the memo key.
#[derive(Clone, Debug, Default)]
pub struct RawRcFlags {
    pub hostname: Option<String>,
    pub label: Vec<String>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub privileged: bool,
    pub tty: bool,
    pub init: bool,
    pub read_only: bool,
    pub oom_score_adj: Option<i32>,
    pub pids_limit: Option<i64>,
    pub shm_size: Option<String>,
    /// WP-#106: `--apparmor <profile>` — the AppArmor profile name (or
    /// "unconfined") to exec the container under. ns-engine only (honest-errored
    /// elsewhere). RUNTIME-ONLY.
    pub apparmor: Option<String>,
    /// WP-#108: `--seccomp <path>` — the PATH to an OCI seccomp JSON profile (or
    /// "unconfined") to enforce on the container. ns-engine only (honest-errored
    /// elsewhere). RUNTIME-ONLY.
    pub seccomp: Option<String>,
}

/// The resolved WP-RC-FLAGS config: `--label` parsed to `(key,value)` pairs and
/// `--shm-size` parsed to bytes; the rest pass through. Lowered into the
/// `RunSpec` carry-fields by the handler. All fields are RUNTIME-ONLY.
#[derive(Clone, Debug, Default)]
pub struct RcConfig {
    pub hostname: Option<String>,
    pub labels: Vec<(String, String)>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub privileged: bool,
    pub tty: bool,
    pub init: bool,
    pub read_only: bool,
    pub oom_score_adj: Option<i32>,
    pub pids_limit: Option<i64>,
    pub shm_size: Option<u64>,
    /// WP-#106: `--apparmor <profile>` — passes through unparsed (the profile name
    /// is a free string; "unconfined" is the only special token, handled by the ns
    /// engine). ns-engine only. RUNTIME-ONLY.
    pub apparmor: Option<String>,
    /// WP-#108: `--seccomp <path>` — passes through unparsed (a profile PATH;
    /// "unconfined" is the only special token, handled by the ns engine, which
    /// compiles the file before exec). ns-engine only. RUNTIME-ONLY.
    pub seccomp: Option<String>,
}

impl RawRcFlags {
    /// Parse the raw flags into a [`RcConfig`]. `--label` and `--shm-size` are
    /// validated (fail-closed: bad input ⇒ printed error + `Err(exit_code)`,
    /// mirroring the other run-flag parsers). All-default raw flags resolve to an
    /// all-default `RcConfig` (behavior-preserving: no flag set ⇒ no-op carry).
    pub fn resolve(self) -> Result<RcConfig, i32> {
        let mut labels: Vec<(String, String)> = Vec::new();
        for raw in &self.label {
            labels.push(parse_label(raw)?);
        }
        let shm_size = match self.shm_size.as_deref() {
            None => None,
            Some(s) => Some(parse_shm_size(s)?),
        };
        Ok(RcConfig {
            hostname: self.hostname,
            labels,
            cap_add: self.cap_add,
            cap_drop: self.cap_drop,
            privileged: self.privileged,
            tty: self.tty,
            init: self.init,
            read_only: self.read_only,
            oom_score_adj: self.oom_score_adj,
            pids_limit: self.pids_limit,
            shm_size,
            apparmor: self.apparmor,
            seccomp: self.seccomp,
        })
    }
}

/// The `--health-*` CLI flags, bundled (WP-RC-4). Built from the parsed `Cmd`
/// in dispatch and lowered to a [`Healthcheck`] by [`HealthFlags::build`].
///
/// `cmd == None` (no `--health-cmd`) OR `no_healthcheck == true` ⇒ no
/// healthcheck (the latter is Docker's `--no-healthcheck`, which wins over any
/// other `--health-*` flag). Otherwise the flags lower 1:1 to a [`Healthcheck`].
#[derive(Clone, Debug, Default)]
pub struct HealthFlags {
    pub cmd: Option<String>,
    pub interval: u64,
    pub timeout: u64,
    pub start_period: u64,
    pub retries: u32,
    pub no_healthcheck: bool,
}

impl HealthFlags {
    /// Lower the flags to a [`Healthcheck`], or `None` when no healthcheck is
    /// configured. `--no-healthcheck` disables unconditionally (Docker
    /// semantics); a missing `--health-cmd` is also "no healthcheck".
    pub fn build(&self) -> Option<Healthcheck> {
        if self.no_healthcheck {
            return None;
        }
        let cmd = self.cmd.clone()?;
        Some(Healthcheck {
            cmd,
            interval_s: self.interval,
            timeout_s: self.timeout,
            start_period_s: self.start_period,
            retries: self.retries,
        })
    }
}

/// WP-NET-ISO: map `--net` to `net_isolate`. `host` ⇒ false (share host
/// network, the default/current behavior); `none` ⇒ true (isolated netns).
/// Any other value is an honest error to stderr + `Err(2)` (fail-closed).
pub(crate) fn net_isolate_from_str(net: &str) -> Result<bool, i32> {
    match net {
        "host" => Ok(false),
        "none" => Ok(true),
        other => {
            eprintln!("lightr: invalid --net value '{other}' (expected host|none)");
            Err(2)
        }
    }
}

/// WP-NET-ISO: resolve `--net` AND enforce that `--net=none` has a netns to
/// create. `is_pure_native` is `native engine && no rootfs` — that path has no
/// netns, so isolation is an honest exit 2 (never a silently-shared host net).
/// Only ns (or vz, via its VM) can give a netns. Returns the `net_isolate` bool.
pub(crate) fn resolve_net_isolate(net: &str, is_pure_native: bool) -> Result<bool, i32> {
    let net_isolate = net_isolate_from_str(net)?;
    if net_isolate && is_pure_native {
        eprintln!("lightr: --net=none (network isolation) requires --engine ns or vz");
        return Err(2);
    }
    Ok(net_isolate)
}

#[cfg(test)]
#[path = "flags_rc_tests.rs"]
mod rc_tests;

#[cfg(test)]
#[path = "flags_publish_tests.rs"]
mod publish_tests;
