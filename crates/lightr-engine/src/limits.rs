//! Resource-limit application for the engine spawns (F-203).
//!
//! build-spec-parity.md §2.2–2.3 — WP-A1 fills these bodies. This is a sibling
//! of `lightr-run`'s `limits.rs`: run's applies to a std `Command` pre-output;
//! engine's applies to the engine's own spawns. The two share identical native
//! semantics; the only honest difference is per-engine reachability of the
//! enforcement mechanism.
//!
//! Honest-boundary law (no silent no-op for an unenforceable cap):
//!   * native  — `memory_bytes` is enforced via `setrlimit(RLIMIT_AS|RLIMIT_DATA)`
//!     in a `pre_exec` hook. `cpu_millis` is NOT faithfully enforceable natively
//!     (`RLIMIT_CPU` is total cpu-seconds, not a share) ⇒ honest `Err`.
//!   * ns      — cgroup v2 `memory.max` / `cpu.max`; honest `Err` if cgroup v2 is
//!     unavailable or the write is denied (no delegation / no `CAP_SYS_RESOURCE`).

use lightr_core::{LightrError, Result};

/// Validate native-engine cap enforceability for THIS OS — honest `Err`, never a
/// silent no-op (build-spec-parity.md §0/§2.2). `cpu_millis` (a share) is not
/// enforceable on any native engine; `memory_bytes` only on Linux (macOS ignores
/// `RLIMIT_AS`/`DATA` — verified `EINVAL`; the hard macOS cap is `--engine vz`).
pub fn check_native_support(limits: &lightr_core::ResourceLimits) -> Result<()> {
    if limits.cpu_millis.is_some() {
        return Err(LightrError::InvalidRef(
            "native engine cannot enforce a cpu share; use --engine ns (cgroup) or vz (vcpu count)"
                .to_string(),
        ));
    }
    // A pids cap needs cgroup v2 `pids.max`, which the native host process cannot
    // create. Honest `Err`, never a silent no-op (the lying-comment bug WP-#90
    // closed). Enforced on the `ns` engine; recorded-only as the native carry-field.
    if limits.pids_max.is_some() {
        return Err(LightrError::InvalidRef(
            "native engine cannot enforce a pids limit; use --engine ns (cgroup)".to_string(),
        ));
    }
    #[cfg(not(target_os = "linux"))]
    if limits.memory_bytes.is_some() {
        return Err(LightrError::InvalidRef(
            "memory limits are not enforceable on the native engine on this OS; \
             use --engine vz (macOS) for a hard cap"
                .to_string(),
        ));
    }
    Ok(())
}

/// Apply resource caps to a not-yet-spawned engine `Command` (native engine).
/// Validates first ([`check_native_support`]); on Linux installs a `pre_exec`
/// `setrlimit` hook for `memory_bytes`. Off Linux only the unlimited case reaches
/// the install (the check already returned an honest `Err` for any cap).
pub fn apply_native(
    cmd: &mut std::process::Command,
    limits: &lightr_core::ResourceLimits,
) -> Result<()> {
    check_native_support(limits)?;
    #[cfg(target_os = "linux")]
    if let Some(memory_bytes) = limits.memory_bytes {
        install_memory_rlimit(cmd, memory_bytes);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cmd;
    }
    Ok(())
}

/// Install a `pre_exec` hook that caps the child's address space (`RLIMIT_AS`)
/// and data segment (`RLIMIT_DATA`) at `memory_bytes`.
///
/// Safety: `pre_exec` runs in the forked child *before* `execvp`. The closure
/// touches only `setrlimit`, which is async-signal-safe, and captures a single
/// `u64` by copy — it performs no allocation and shares no locks with the parent.
///
/// Linux-only: macOS ignores `RLIMIT_AS`/`RLIMIT_DATA` (`check_native_support`
/// already rejected a macOS memory cap before we reach here).
#[cfg(target_os = "linux")]
fn install_memory_rlimit(cmd: &mut std::process::Command, memory_bytes: u64) {
    use std::os::unix::process::CommandExt;

    // SAFETY: see the doc comment — the hook is allocation-free and only calls
    // the async-signal-safe `setrlimit`.
    unsafe {
        cmd.pre_exec(move || {
            let lim = libc::rlimit {
                rlim_cur: memory_bytes as libc::rlim_t,
                rlim_max: memory_bytes as libc::rlim_t,
            };
            // RLIMIT_AS: total virtual address space. The primary memory cap.
            if libc::setrlimit(libc::RLIMIT_AS, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // RLIMIT_DATA: data segment (brk/sbrk + anonymous mmap on Linux).
            if libc::setrlimit(libc::RLIMIT_DATA, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Apply resource caps via cgroup v2 (the `ns` engine, Linux).
///
/// Writes a transient cgroup under the caller's delegated cgroup-v2 subtree:
/// `memory.max` ← `memory_bytes`, `cpu.max` ← `"<millis*100> 100000"` (quota
/// space-period). The current process is moved into the new cgroup so the
/// subsequent `exec` inherits the caps. If cgroup v2 is unavailable, or any write
/// is denied (no delegation / no `CAP_SYS_RESOURCE`), returns an honest `Err`;
/// it never silently pretends to enforce.
///
/// WP-#99: `cgroup_name` lets the caller pin an EXPLICIT leaf name. When `Some`,
/// the leaf is created and joined EVEN IF the limits are unlimited — so the
/// process lands in a known, killable cgroup (the CRI backend's `stop` writes
/// that leaf's `cgroup.kill`). When `None`, behavior is unchanged: an unlimited
/// run is a no-op, a limited run uses the transient `lightr.<pid>` leaf.
#[cfg(target_os = "linux")]
pub fn apply_cgroup(
    limits: &lightr_core::ResourceLimits,
    cgroup_name: Option<&str>,
) -> Result<()> {
    if cgroup_name.is_none() && limits.is_unlimited() {
        return Ok(());
    }
    cgroup_v2::apply(limits, cgroup_name)
}

/// Non-Linux: cgroups do not exist. Honest `Err` when a cap is asked for; inert
/// `Ok(())` when unlimited. (The `ns` engine itself is Linux-only — this arm
/// only exists so the symbol resolves on every target.)
#[cfg(not(target_os = "linux"))]
pub fn apply_cgroup(
    limits: &lightr_core::ResourceLimits,
    cgroup_name: Option<&str>,
) -> Result<()> {
    // An explicit leaf name is a Linux-only cgroup-v2 concept; off Linux it is
    // simply not honored (the ns engine itself is Linux-only). Unlimited + no
    // name ⇒ inert Ok; any cap or named leaf ⇒ honest Unsupported.
    if cgroup_name.is_none() && limits.is_unlimited() {
        return Ok(());
    }
    Err(LightrError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "cgroup limits require Linux cgroup v2",
    )))
}

#[cfg(target_os = "linux")]
mod cgroup_v2 {
    use lightr_core::{LightrError, Result};
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};

    const CGROUP_ROOT: &str = "/sys/fs/cgroup";

    /// Create a cgroup, write the caps, and join it. Honest `Err` on any failure
    /// (no v2 mount, not delegated, write denied).
    ///
    /// WP-#99: `cgroup_name`, when `Some`, names the leaf EXPLICITLY (the CRI
    /// backend pins `lightr-cri/<cid>` so its `stop` can `cgroup.kill` the whole
    /// subtree). `None` keeps the transient `lightr.<pid>` leaf (unique per
    /// process so concurrent runs don't collide).
    pub fn apply(
        limits: &lightr_core::ResourceLimits,
        cgroup_name: Option<&str>,
    ) -> Result<()> {
        // cgroup v2 presents a unified hierarchy with a `cgroup.controllers`
        // file at the root. Its absence ⇒ v1 / not mounted ⇒ honest Unsupported.
        let root = Path::new(CGROUP_ROOT);
        if !root.join("cgroup.controllers").exists() {
            return Err(unsupported(
                "cgroup v2 unified hierarchy not mounted at /sys/fs/cgroup",
            ));
        }

        // The leaf: an explicit name (CRI; may be a nested `a/b` path) or the
        // transient per-process default.
        let leaf: PathBuf = match cgroup_name {
            Some(name) => root.join(name),
            None => root.join(format!("lightr.{}", std::process::id())),
        };
        std::fs::create_dir_all(&leaf).map_err(|e| {
            denied_or_io(
                e,
                "cannot create a cgroup (subtree not delegated / no CAP_SYS_RESOURCE)",
            )
        })?;

        if let Some(bytes) = limits.memory_bytes {
            write_ctl(&leaf, "memory.max", &bytes.to_string())?;
        }
        if let Some(millis) = limits.cpu_millis {
            // cpu.max = "<quota> <period>"; quota = millis * 100 over a 100000µs
            // period (1000 millis == 1 full core == "100000 100000").
            let quota = millis.saturating_mul(100);
            write_ctl(&leaf, "cpu.max", &format!("{quota} 100000"))?;
        }
        if let Some(p) = limits.pids_max {
            // pids.max caps the live process/thread count in the cgroup (Docker
            // `--pids-limit`). A fork beyond it fails with EAGAIN in the guest.
            write_ctl(&leaf, "pids.max", &p.to_string())?;
        }

        // Join: write our PID into the leaf's cgroup.procs so exec inherits caps.
        write_ctl(&leaf, "cgroup.procs", &std::process::id().to_string())?;
        Ok(())
    }

    fn write_ctl(dir: &Path, file: &str, value: &str) -> Result<()> {
        let path = dir.join(file);
        std::fs::write(&path, value.as_bytes())
            .map_err(|e| denied_or_io(e, &format!("cannot write cgroup file {}", path.display())))
    }

    /// Map a write/permission failure to an honest error, distinguishing a
    /// denial (no delegation / caps) from a generic I/O fault.
    fn denied_or_io(e: std::io::Error, ctx: &str) -> LightrError {
        match e.kind() {
            ErrorKind::PermissionDenied => unsupported(ctx),
            _ => LightrError::Io(std::io::Error::new(e.kind(), format!("{ctx}: {e}"))),
        }
    }

    fn unsupported(msg: &str) -> LightrError {
        LightrError::Io(std::io::Error::new(ErrorKind::Unsupported, msg.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightr_core::ResourceLimits;

    // unlimited ⇒ never errors, regardless of platform.
    #[test]
    fn apply_native_unlimited_ok() {
        let mut cmd = std::process::Command::new("/bin/true");
        assert!(apply_native(&mut cmd, &ResourceLimits::default()).is_ok());
    }

    // A cpu share on the native engine is an honest error (no silent ignore).
    #[cfg(unix)]
    #[test]
    fn apply_native_cpu_share_is_honest_err() {
        let mut cmd = std::process::Command::new("/bin/true");
        let limits = ResourceLimits {
            memory_bytes: None,
            cpu_millis: Some(500),
            pids_max: None,
        };
        let err = apply_native(&mut cmd, &limits).expect_err("cpu share must error");
        assert!(
            err.to_string().contains("cpu share"),
            "expected a cpu-share error, got: {err}"
        );
    }

    // A pids cap on the native engine is an honest error (cgroup-only, no silent
    // no-op — the WP-#90 fix). Enforced on `ns`; recorded-only as a native field.
    #[cfg(unix)]
    #[test]
    fn apply_native_pids_limit_is_honest_err() {
        let mut cmd = std::process::Command::new("/bin/true");
        let limits = ResourceLimits::default().with_pids(Some(16));
        let err = apply_native(&mut cmd, &limits).expect_err("pids limit must error");
        assert!(
            err.to_string().contains("pids limit"),
            "expected a pids-limit error, got: {err}"
        );
    }

    // A memory cap installs the pre_exec hook on Linux (RLIMIT_AS/DATA honored);
    // off Linux it is an honest Err (macOS/Windows can't enforce it natively).
    #[cfg(target_os = "linux")]
    #[test]
    fn apply_native_memory_ok_on_linux() {
        let mut cmd = std::process::Command::new("/bin/true");
        let limits = ResourceLimits {
            memory_bytes: Some(64 * 1024 * 1024),
            cpu_millis: None,
            pids_max: None,
        };
        assert!(apply_native(&mut cmd, &limits).is_ok());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn apply_native_memory_honest_err_off_linux() {
        let mut cmd = std::process::Command::new("/bin/true");
        let limits = ResourceLimits {
            memory_bytes: Some(64 * 1024 * 1024),
            cpu_millis: None,
            pids_max: None,
        };
        let err = apply_native(&mut cmd, &limits).expect_err("memory cap must error off-Linux");
        assert!(
            err.to_string().contains("not enforceable"),
            "expected an honest 'not enforceable' error, got: {err}"
        );
    }

    // apply_cgroup with unlimited + no explicit leaf name never errors (it is a
    // no-op fast path).
    #[test]
    fn apply_cgroup_unlimited_ok() {
        assert!(apply_cgroup(&ResourceLimits::default(), None).is_ok());
    }
}
