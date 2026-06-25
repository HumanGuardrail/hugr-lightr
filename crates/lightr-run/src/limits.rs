//! Resource-limit application for the memoized native spawn (F-203).
//!
//! build-spec-parity.md §2.2–2.3 — WP-A1 fills these bodies. This is the
//! memoized-run sibling of `lightr-engine`'s `limits.rs`: this one applies to
//! the std `Command` built in `run_memoized_with` *before* `.output()`; the
//! engine's applies to the engine's own spawns. The native semantics are
//! identical.
//!
//! Honest-boundary law (no silent no-op for an unenforceable cap):
//!   * `memory_bytes` is enforced via `setrlimit(RLIMIT_AS|RLIMIT_DATA)` in a
//!     `pre_exec` hook (macOS + Linux). An over-cap child is killed by the kernel.
//!   * `cpu_millis` is NOT faithfully enforceable on the native path
//!     (`RLIMIT_CPU` is total cpu-seconds, not a share) ⇒ honest `Err`. The
//!     memoized run is the native path, so a cpu share is always an honest error
//!     here (use `--engine ns`/`vz`).

use lightr_core::{LightrError, ResourceLimits, Result};

/// Validate that the requested caps are enforceable on the **native** engine for
/// THIS OS — fail closed with an honest error, never a silent no-op. Call EARLY
/// (before the AC lookup) so a cache-HIT can't bypass the honest error
/// (build-spec-parity.md §0/§2.2 — the memo key excludes limits).
///
/// * `cpu_millis` (a cpu *share*) is not faithfully enforceable on any native
///   engine (`RLIMIT_CPU` caps total cpu-seconds, not a share) ⇒ honest `Err`.
/// * `memory_bytes` is enforceable natively ONLY on Linux (`RLIMIT_AS`/`DATA`).
///   macOS (Darwin) silently ignores those rlimits (verified: `setrlimit` returns
///   `EINVAL`, an over-cap alloc succeeds) and there is no stable PUBLIC macOS
///   mechanism (jetsam/Mach footprint are private) ⇒ honest `Err` ⇒ use
///   `--engine vz` (the VM sets a hard RAM cap — Docker's own mechanism on Mac).
pub fn check_native_support(limits: &ResourceLimits) -> Result<()> {
    if limits.cpu_millis.is_some() {
        return Err(LightrError::InvalidRef(
            "native engine cannot enforce a cpu share; use --engine ns (cgroup) or vz (vcpu count)"
                .to_string(),
        ));
    }
    // A pids cap needs cgroup v2 `pids.max` — unavailable to the native host
    // process. Honest `Err` (WP-#90), never a silent no-op; enforced on `ns`.
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

/// Apply resource caps to a not-yet-spawned `Command` (memoized native path).
/// Validates first ([`check_native_support`] — honest `Err` for any cap this OS
/// can't enforce); on Linux installs a `pre_exec` `setrlimit` hook for
/// `memory_bytes`. Off Linux the check has already returned `Err` for any cap,
/// so only the unlimited case reaches here (no-op).
pub fn apply_native(cmd: &mut std::process::Command, limits: &ResourceLimits) -> Result<()> {
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
/// The memoized run uses the native spawn, so this path is not reached from
/// `run_memoized_with`; the symbol is kept (mirroring `lightr-engine`) so the
/// frozen seam stays consistent. Writes a transient cgroup — `memory.max` ←
/// bytes, `cpu.max` ← `"<millis*100> 100000"` — and joins it, or returns an
/// honest `Err` if cgroup v2 is unavailable / the write is denied.
#[cfg(target_os = "linux")]
pub fn apply_cgroup(limits: &ResourceLimits) -> Result<()> {
    if limits.is_unlimited() {
        return Ok(());
    }
    cgroup_v2::apply(limits)
}

/// Non-Linux: cgroups do not exist. Honest `Err` when a cap is asked for; inert
/// `Ok(())` when unlimited.
#[cfg(not(target_os = "linux"))]
pub fn apply_cgroup(limits: &ResourceLimits) -> Result<()> {
    if limits.is_unlimited() {
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

    /// Create a transient cgroup, write the caps, and join it. Honest `Err` on
    /// any failure (no v2 mount, not delegated, write denied).
    pub fn apply(limits: &lightr_core::ResourceLimits) -> Result<()> {
        let root = Path::new(CGROUP_ROOT);
        if !root.join("cgroup.controllers").exists() {
            return Err(unsupported(
                "cgroup v2 unified hierarchy not mounted at /sys/fs/cgroup",
            ));
        }

        let leaf: PathBuf = root.join(format!("lightr.{}", std::process::id()));
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
            let quota = millis.saturating_mul(100);
            write_ctl(&leaf, "cpu.max", &format!("{quota} 100000"))?;
        }
        if let Some(p) = limits.pids_max {
            write_ctl(&leaf, "pids.max", &p.to_string())?;
        }

        write_ctl(&leaf, "cgroup.procs", &std::process::id().to_string())?;
        Ok(())
    }

    fn write_ctl(dir: &Path, file: &str, value: &str) -> Result<()> {
        let path = dir.join(file);
        std::fs::write(&path, value.as_bytes())
            .map_err(|e| denied_or_io(e, &format!("cannot write cgroup file {}", path.display())))
    }

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

    #[test]
    fn apply_native_unlimited_ok() {
        let mut cmd = std::process::Command::new("/bin/true");
        assert!(apply_native(&mut cmd, &ResourceLimits::default()).is_ok());
    }

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

    // A pids cap on the native memo path is an honest error (cgroup-only; WP-#90).
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

    #[test]
    fn apply_cgroup_unlimited_ok() {
        assert!(apply_cgroup(&ResourceLimits::default()).is_ok());
    }
}
